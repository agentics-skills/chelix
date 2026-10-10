import { chatAddMsg, resetChatView, setComposerStopButton, updateTokenBar } from "../chat-ui";
import { sendRpc } from "../helpers";
import { clearQueuedPromptsDock, replaceQueuedPromptsDock } from "../pages/chat/prompt-queue";
import { updateSessionProjectSelect } from "../project-combo";
import { currentPrefix, navigate, sessionPath } from "../router";
import { routes } from "../routes";
import * as S from "../state";
import { projectStore } from "../stores/project-store";
import { clearSessionHistory, getSessionHistory } from "../stores/session-history-cache";
import { insertSessionInOrder, type Session, sessionStore } from "../stores/session-store";
import {
	type PendingToolPermission,
	rememberPendingToolPermissions,
	syncToolPermissionToolbar,
} from "../tool-permission";
import type { RpcResponse } from "../types/rpc";
import type { SessionMeta } from "../types/session";
import type { QueuedPromptsStatus } from "../types/ws-events";
import { invalidateHistorySubscription, subscribeSessionHistory } from "./history-subscription";
import { clearPendingSends } from "./pending-send";
import { setSearchNavigation, takeSearchNavigation } from "./search-navigation";
import {
	restoreSessionModelSettings as restoreSessionModelSettingsImpl,
	setSessionAgent as setSessionAgentImpl,
} from "./session-agent";
import { renderSessionHistory } from "./session-history-pagination";
import {
	hideSessionLoadIndicator,
	type SearchContext,
	showSessionLoadIndicator,
	updateChatSessionHeader,
} from "./session-render";

export const restoreSessionModelSettings = restoreSessionModelSettingsImpl;
export const setSessionAgent = setSessionAgentImpl;

function focusChatInputIfIdle(): void {
	const element = document.activeElement;
	if (
		element &&
		element !== document.body &&
		element !== S.chatInput &&
		(element.tagName === "INPUT" || element.tagName === "TEXTAREA" || (element as HTMLElement).isContentEditable)
	)
		return;
	S.chatInput?.focus();
}

export function restoreSessionState(entry: SessionMeta, projectId?: string): void {
	const effectiveProjectId = entry.projectId || projectId || "";
	projectStore.setActiveProjectId(effectiveProjectId);
	S.setActiveProjectId(effectiveProjectId);
	localStorage.setItem("chelix-project", effectiveProjectId);
	updateSessionProjectSelect(effectiveProjectId);
	restoreSessionModelSettings(entry);
	updateChatSessionHeader();
}

interface SwitchPayload {
	entry: SessionMeta;
	replying: boolean;
	voicePending: boolean;
	queuedPrompts: QueuedPromptsStatus;
	pendingToolPermissions?: PendingToolPermission[];
}

let switchRequest = 0;

function resetSwitchViewState(): void {
	hideSessionLoadIndicator();
	if (S.chatMsgBox) resetChatView(S.chatMsgBox);
	clearQueuedPromptsDock();
	S.setStreamEl(null);
	S.setStreamText("");
	S.setLastToolOutput("");
	S.setVoicePending(false);
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	S.setSessionContextWindow(0);
	setComposerStopButton(false);
	updateTokenBar(null);
}

export function clearActiveSession(): Promise<RpcResponse> {
	const key = S.activeSessionKey;
	return sendRpc("chat.clear", {}).then((response) => {
		if (!response.ok && key === S.activeSessionKey) chatAddMsg("error", response.error?.message || "Clear failed");
		return response;
	});
}

function applySwitchMetadata(key: string, payload: SwitchPayload, projectId?: string): void {
	const entry = sessionStore.upsert({ ...payload.entry, key });
	if (!entry) throw new Error("Invalid session metadata");
	if (!(S.sessions as SessionMeta[]).some((session) => session.key === key))
		S.setSessions(insertSessionInOrder(S.sessions as Session[], entry));
	restoreSessionState(payload.entry, projectId);
	entry.replying.value = payload.replying;
	entry.voicePending.value = payload.voicePending;
	S.setVoicePending(payload.voicePending);
	setComposerStopButton(payload.replying, key);
	replaceQueuedPromptsDock(payload.queuedPrompts);
	rememberPendingToolPermissions(payload.pendingToolPermissions);
	syncToolPermissionToolbar();
}

function validateSearchGeneration(context: SearchContext | null | undefined, generation: string): void {
	if (context && context.generation !== generation)
		throw new Error("Search result belongs to an expired session generation");
}

const requestedCreations = new Set<string>();
let pendingSwitchProject: { key: string; projectId: string } | null = null;

export function prepareNewSessionKey(): string {
	const key = `session:${crypto.randomUUID()}`;
	requestedCreations.add(key);
	return key;
}

export function showUnselectedSession(): void {
	clearPendingSends();
	invalidateHistorySubscription();
	clearSessionHistory();
	sessionStore.setActive("");
	S.setActiveSessionKey("");
	const path = routes.chats;
	const onChats = currentPrefix === path || location.pathname === path || location.pathname.startsWith(`${path}/`);
	if (!onChats) return;
	if (location.pathname !== path) navigate(path);
}

function redirectMissingSession(key: string, response: RpcResponse): boolean {
	if (response.ok || response.error?.code !== "NOT_FOUND") return false;
	sessionStore.remove(key);
	S.setSessions((S.sessions as SessionMeta[]).filter((session) => session.key !== key));
	showUnselectedSession();
	return true;
}

export function switchSession(key: string, searchContext?: SearchContext | null, projectId?: string): void {
	if (!S.chatMsgBox) {
		if (searchContext) setSearchNavigation(key, searchContext);
		if (projectId) pendingSwitchProject = { key, projectId };
		navigate(sessionPath(key));
		return;
	}
	const create = requestedCreations.delete(key);
	const switchedProjectId =
		projectId ?? (pendingSwitchProject?.key === key ? pendingSwitchProject.projectId : undefined);
	if (pendingSwitchProject?.key === key) pendingSwitchProject = null;
	clearPendingSends();
	searchContext ||= takeSearchNavigation(key);
	const request = ++switchRequest;
	invalidateHistorySubscription();
	clearSessionHistory();
	sessionStore.setActive(key);
	S.setActiveSessionKey(key);
	history.replaceState(null, "", sessionPath(key));
	resetSwitchViewState();
	showSessionLoadIndicator();
	sessionStore.refreshInProgressKey.value = key;
	void (async () => {
		const response = await sendRpc<SwitchPayload>("sessions.switch", {
			key,
			create,
			include_history: false,
			...(switchedProjectId ? { project_id: switchedProjectId } : {}),
		});
		if (request !== switchRequest) return;
		if (redirectMissingSession(key, response)) return;
		if (!(response.ok && response.payload?.entry)) throw new Error(response.error?.message || "Failed to load session");
		const payload = response.payload;
		applySwitchMetadata(key, payload, switchedProjectId);
		const page = await subscribeSessionHistory(key, searchContext?.messageId);
		if (request !== switchRequest || !page) return;
		validateSearchGeneration(searchContext, page.generation);
		renderSessionHistory(key, getSessionHistory(key) || [], searchContext || null, page.totalMessages, false);
		focusChatInputIfIdle();
	})()
		.catch((error: unknown) => {
			if (request !== switchRequest) return;
			hideSessionLoadIndicator();
			chatAddMsg("error", error instanceof Error ? error.message : "Failed to load session");
		})
		.finally(() => {
			if (request === switchRequest) sessionStore.refreshInProgressKey.value = "";
		});
}
