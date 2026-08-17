// ── Session switching: switch, restore, refresh ─────────────────

import { chatAddMsg, setComposerStopButton, updateTokenBar } from "../chat-ui";
import { unmountExecuteCommandToolBubbles } from "../components/ExecuteCommandToolBubble";
import { sendRpc } from "../helpers";
import { renderQueuedPrompts, setQueuedPrompts } from "../pages/chat/prompt-queue";
import { updateSessionProjectSelect } from "../project-combo";
import { sessionPath } from "../router";
import * as S from "../state";
import { projectStore } from "../stores/project-store";
import {
	clearSessionHistory,
	getHistoryRevision,
	getSessionHistory,
	replaceSessionHistory,
} from "../stores/session-history-cache";
import { insertSessionInOrder, Session, sessionStore } from "../stores/session-store";
import { isToolLifecycleEvent, reduceToolInvocation } from "../tool-lifecycle";
import type { RpcResponse } from "../types/rpc";
import type { HistoryMessage, SessionMeta } from "../types/session";
import type { ActiveToolInvocation, QueuedPrompt, ReasoningContent } from "../types/ws-events";
import { clearToolLifecycleStateForSession, renderToolLifecycleSnapshot } from "../ws/tool-helpers";

import {
	restoreSessionModelSettings as restoreSessionModelSettingsImpl,
	setSessionAgent as setSessionAgentImpl,
} from "./session-agent";
import {
	clearHistoryPaginationState,
	fetchSessionHistoryViaHttp,
	getHistoryPaginationState,
	type HistoryPayload,
	isHistoryCacheComplete,
	SESSION_HISTORY_PAGE_LIMIT,
	setHistoryPaginationState,
	shouldApplyServerHistory,
} from "./session-history";

import { renderSessionHistory } from "./session-history-pagination";
import { markSessionLocallyCleared } from "./session-list";
import {
	hideSessionLoadIndicator,
	postHistoryLoadActions,
	type SearchContext,
	showSessionLoadIndicator,
	updateChatSessionHeader,
} from "./session-render";

export const restoreSessionModelSettings = restoreSessionModelSettingsImpl;
export const setSessionAgent = setSessionAgentImpl;

/** Focus the chat input unless the user is actively editing a text field
 *  (e.g. rename input, search field). Buttons and other non-text elements
 *  are fine to steal focus from. */
function focusChatInputIfIdle(): void {
	const el = document.activeElement;
	if (el && el !== document.body && el !== S.chatInput) {
		const tag = el.tagName;
		if (tag === "INPUT" || tag === "TEXTAREA" || (el as HTMLElement).isContentEditable) return;
	}
	S.chatInput?.focus();
}

// ── Types ────────────────────────────────────────────────────

interface SwitchPayload {
	entry?: SessionMeta;
	history?: HistoryMessage[];
	historyCacheHit?: boolean;
	historyTruncated?: boolean;
	historyDroppedCount?: number;
	historyOmitted?: boolean;
	replying?: boolean;
	thinkingText?: ReasoningContent;
	voicePending?: boolean;
	activeToolInvocations?: ActiveToolInvocation[];
	queuedPrompts?: QueuedPrompt[];
	hasMore?: boolean;
	nextCursor?: number;
	totalMessages?: number;
}

/** Parameters for the sessions.switch RPC call. */
interface SwitchRpcParams {
	key: string;
	project_id?: string;
	include_history?: boolean;
}

interface SwitchRequestContext {
	key: string;
	searchContext: SearchContext | null;
	projectId?: string;
	requestId: number;
	hasCache: boolean;
	cachedHistoryCount: number | null;
	cacheRevisionAtRequest: number;
}

interface HistoryApplication {
	appliedServerHistory: boolean;
	resolvedHistory: HistoryMessage[];
}

interface PaginationSnapshot {
	hasMore: boolean;
	nextCursor: number | null;
}

// ── Module state ─────────────────────────────────────────────

let switchRequestSeq = 0;
const latestSwitchRequestBySession = new Map<string, number>();

// ── MCP toggle restore ──────────────────────────────────────
function restoreMcpToggle(mcpEnabled: boolean): void {
	const mcpBtn = S.$("mcpToggleBtn");
	const mcpLabel = S.$("mcpToggleLabel");
	if (mcpBtn) {
		mcpBtn.style.color = mcpEnabled ? "var(--ok)" : "var(--muted)";
		mcpBtn.style.borderColor = mcpEnabled ? "var(--ok)" : "var(--border)";
	}
	if (mcpLabel) mcpLabel.textContent = mcpEnabled ? "MCP" : "MCP off";
}

// ── Restore session state ───────────────────────────────────

export function restoreSessionState(entry: SessionMeta, projectId?: string): void {
	const effectiveProjectId = entry.projectId || projectId || "";
	projectStore.setActiveProjectId(effectiveProjectId);
	S.setActiveProjectId(effectiveProjectId);
	localStorage.setItem("chelix-project", effectiveProjectId);
	updateSessionProjectSelect(effectiveProjectId);
	restoreSessionModelSettings(entry);
	restoreMcpToggle(!entry.mcpDisabled);
	updateChatSessionHeader();
}

// ── Switch request tracking ─────────────────────────────────

export function startSwitchRequest(key: string): number {
	switchRequestSeq += 1;
	latestSwitchRequestBySession.set(key, switchRequestSeq);
	return switchRequestSeq;
}

function isLatestSwitchRequest(key: string, requestId: number): boolean {
	return latestSwitchRequestBySession.get(key) === requestId;
}

export function startSessionRefresh(key: string, blockRealtimeEvents: boolean): void {
	sessionStore.refreshInProgressKey.value = key;
	sessionStore.switchInProgress.value = !!blockRealtimeEvents;
	S.setSessionSwitchInProgress(!!blockRealtimeEvents);
}

function finishSessionRefresh(key: string): void {
	if (sessionStore.refreshInProgressKey.value !== key) return;
	sessionStore.refreshInProgressKey.value = "";
	sessionStore.switchInProgress.value = false;
	S.setSessionSwitchInProgress(false);
}

function resetSwitchViewState(): void {
	hideSessionLoadIndicator();
	if (S.chatMsgBox) {
		unmountExecuteCommandToolBubbles(S.chatMsgBox);
		S.chatMsgBox.textContent = "";
	}
	renderQueuedPrompts();
	S.setStreamEl(null);
	S.setStreamText("");
	S.setLastToolOutput("");
	S.setVoicePending(false);
	S.setLastHistoryIndex(-1);
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	S.setSessionContextWindow(0);
	setComposerStopButton(false);
	updateTokenBar();
}

function ensureSessionInClientStore(key: string, entry: SessionMeta, projectId?: string): unknown {
	const existing = sessionStore.getByKey(key);
	if (existing) return existing;

	const created: SessionMeta = { ...entry, key };
	if (projectId && !created.projectId) created.projectId = projectId;
	const createdSession = sessionStore.upsert(created);

	const inLegacy = (S.sessions as SessionMeta[]).some((s) => s.key === key);
	if (!inLegacy) {
		S.setSessions(insertSessionInOrder(S.sessions as Session[], new Session(created)));
	}
	return createdSession;
}

function applyReplyingStateFromSwitchPayload(key: string, payload: SwitchPayload): void {
	const replying = payload.replying === true;
	const session = sessionStore.getByKey(key);
	if (session) session.replying.value = replying;
	const entry = (S.sessions as SessionMeta[]).find((s) => s.key === key);
	if (entry) entry._replying = replying;

	const voiceSession = sessionStore.getByKey(key);
	if (replying && payload.voicePending) {
		S.setVoicePending(true);
		if (voiceSession) voiceSession.voicePending.value = true;
	} else {
		S.setVoicePending(false);
		if (voiceSession) voiceSession.voicePending.value = false;
	}
	if (!replying && key === sessionStore.activeSessionKey.value && S.streamEl?.classList.contains("reasoning-stream")) {
		S.streamEl.remove();
		S.setStreamEl(null);
		S.setStreamText("");
	}
	if (key === sessionStore.activeSessionKey.value) {
		setComposerStopButton(replying, key);
	}
}

function restoreActiveToolInvocationsFromSwitchPayload(key: string, payload: SwitchPayload): void {
	if (key !== sessionStore.activeSessionKey.value) return;
	clearToolLifecycleStateForSession(key);
	const invocations = Array.isArray(payload.activeToolInvocations) ? payload.activeToolInvocations : [];
	for (const invocation of invocations) {
		if (!isToolLifecycleEvent(invocation)) continue;
		const snapshot = reduceToolInvocation(undefined, invocation, {
			runId: invocation.runId,
			executionMode: invocation.executionMode,
			accumulatedArguments: invocation.accumulatedArguments,
		});
		renderToolLifecycleSnapshot(snapshot, key);
	}
}

/** Clear history for the currently active session and reset local UI state. */
export function clearActiveSession(): Promise<RpcResponse> {
	const prevHistoryIdx = S.lastHistoryIndex;
	const prevSeq = S.chatSeq;
	S.setLastHistoryIndex(-1);
	S.setChatSeq(0);
	return sendRpc("chat.clear", {}).then((res) => {
		if (res?.ok) {
			if (S.chatMsgBox) {
				unmountExecuteCommandToolBubbles(S.chatMsgBox);
				S.chatMsgBox.textContent = "";
			}
			S.setSessionTokens({ input: 0, output: 0 });
			S.setSessionCurrentInputTokens(0);
			S.setSessionCurrentContextTokens(0);
			updateTokenBar();
			const activeKey = sessionStore.activeSessionKey.value || S.activeSessionKey;
			markSessionLocallyCleared(activeKey);
			clearSessionHistory(activeKey);
			clearHistoryPaginationState(activeKey);
			return res;
		}
		S.setLastHistoryIndex(prevHistoryIdx);
		S.setChatSeq(prevSeq);
		chatAddMsg("error", res?.error?.message || "Clear failed");
		return res;
	});
}

// ── Main switch session function ────────────────────────────

function capturePagination(key: string): PaginationSnapshot {
	const paging = getHistoryPaginationState(key);
	return {
		hasMore: paging?.hasMore === true,
		nextCursor: Number.isInteger(paging?.nextCursor) ? (paging?.nextCursor as number) : null,
	};
}

function paginationChanged(before: PaginationSnapshot, after: PaginationSnapshot): boolean {
	return before.hasMore !== after.hasMore || before.nextCursor !== after.nextCursor;
}

function switchHistoryPayload(payload: SwitchPayload): HistoryPayload {
	return {
		historyCacheHit: payload.historyCacheHit === true,
		history: Array.isArray(payload.history) ? payload.history : [],
		historyTruncated: payload.historyTruncated === true,
		historyDroppedCount: Number(payload.historyDroppedCount) || 0,
	};
}

function resolveSwitchHistory(context: SwitchRequestContext, payload: SwitchPayload): Promise<HistoryPayload> {
	if (payload.historyOmitted !== true) return Promise.resolve(switchHistoryPayload(payload));
	return fetchSessionHistoryViaHttp(context.key, {
		cachedMessageCount: context.cachedHistoryCount ?? undefined,
		limit: SESSION_HISTORY_PAGE_LIMIT,
	});
}

function applySwitchHistory(context: SwitchRequestContext, payload: HistoryPayload): HistoryApplication {
	const serverHistory = Array.isArray(payload.history) ? payload.history : [];
	const appliedServerHistory =
		payload.historyCacheHit !== true &&
		shouldApplyServerHistory(context.key, serverHistory, context.cacheRevisionAtRequest);
	if (appliedServerHistory) replaceSessionHistory(context.key, serverHistory);
	return {
		appliedServerHistory,
		resolvedHistory: getSessionHistory(context.key) || serverHistory,
	};
}

function finishSwitchWithError(context: SwitchRequestContext, message: string): void {
	const stillActive = sessionStore.activeSessionKey.value === context.key;
	if (stillActive && !context.hasCache) {
		hideSessionLoadIndicator();
		chatAddMsg("error", message);
	}
	finishSessionRefresh(context.key);
	if (stillActive) focusChatInputIfIdle();
}

function appendHistoryLoadNotices(application: HistoryApplication, historyPayload: HistoryPayload): void {
	if (!application.appliedServerHistory) return;
	if (historyPayload.historyTruncated === true) {
		const dropped = Number(historyPayload.historyDroppedCount) || 0;
		chatAddMsg(
			"system",
			`Loaded the most recent messages for performance (${dropped} older message${dropped === 1 ? "" : "s"} omitted).`,
		);
	}
	if (historyPayload.hasMore === true) {
		const total = Number(historyPayload.totalMessages) || application.resolvedHistory.length;
		chatAddMsg(
			"system",
			`Loaded recent history (${application.resolvedHistory.length} of ${total} messages) for faster loading.`,
		);
	}
}

function renderActiveSwitch(
	context: SwitchRequestContext,
	switchPayload: SwitchPayload,
	entry: SessionMeta,
	historyPayload: HistoryPayload,
	application: HistoryApplication,
	pagingChanged: boolean,
): void {
	restoreSessionState(entry, context.projectId);
	applyReplyingStateFromSwitchPayload(context.key, switchPayload);
	setQueuedPrompts(context.key, switchPayload.queuedPrompts ?? []);
	const thinkingText = switchPayload.replying ? switchPayload.thinkingText || null : null;
	const totalCountHint = Number.isInteger(entry.messageCount)
		? (entry.messageCount as number)
		: Number(historyPayload.totalMessages) || application.resolvedHistory.length;
	const shouldRerender =
		!context.hasCache || Boolean(context.searchContext?.query) || application.appliedServerHistory || pagingChanged;
	if (shouldRerender) {
		renderSessionHistory(
			context.key,
			application.resolvedHistory,
			context.searchContext,
			thinkingText,
			totalCountHint,
			false,
		);
	} else {
		postHistoryLoadActions(context.key, context.searchContext, [], thinkingText, false);
	}
	restoreActiveToolInvocationsFromSwitchPayload(context.key, switchPayload);
	appendHistoryLoadNotices(application, historyPayload);
	focusChatInputIfIdle();
}

async function handleSwitchResponse(
	context: SwitchRequestContext,
	response: RpcResponse<SwitchPayload>,
): Promise<void> {
	if (!isLatestSwitchRequest(context.key, context.requestId)) return;
	if (!(response.ok && response.payload)) {
		finishSwitchWithError(context, response.error?.message || "Failed to load session");
		return;
	}

	const switchPayload = response.payload;
	const entry = switchPayload.entry || ({} as SessionMeta);
	ensureSessionInClientStore(context.key, entry, context.projectId);
	const pagingBefore = capturePagination(context.key);
	let historyPayload: HistoryPayload;
	try {
		historyPayload = await resolveSwitchHistory(context, switchPayload);
	} catch (error) {
		if (!isLatestSwitchRequest(context.key, context.requestId)) return;
		const message = error instanceof Error ? error.message : "Failed to load session history";
		finishSwitchWithError(context, message);
		return;
	}
	if (!isLatestSwitchRequest(context.key, context.requestId)) return;

	setHistoryPaginationState(context.key, historyPayload);
	const pagingChanged = paginationChanged(pagingBefore, capturePagination(context.key));
	const application = applySwitchHistory(context, historyPayload);
	if (sessionStore.activeSessionKey.value === context.key) {
		renderActiveSwitch(context, switchPayload, entry, historyPayload, application, pagingChanged);
	}
	finishSessionRefresh(context.key);
}

export function switchSession(key: string, searchContext?: SearchContext | null, projectId?: string): void {
	sessionStore.setActive(key);
	S.setActiveSessionKey(key);
	localStorage.setItem("chelix-session", key);
	history.replaceState(null, "", sessionPath(key));
	resetSwitchViewState();
	const cachedEntry = sessionStore.getByKey(key);
	if (cachedEntry) restoreSessionState(cachedEntry.toMeta(), projectId);

	const cachedHistory = getSessionHistory(key);
	const hasCache = cachedHistory !== null;
	const cacheComplete = hasCache && isHistoryCacheComplete(key);
	const cachedHistoryCount = cacheComplete
		? Number.isInteger(cachedEntry?.messageCount)
			? (cachedEntry?.messageCount as number)
			: cachedHistory?.length || 0
		: null;
	const context: SwitchRequestContext = {
		key,
		searchContext: searchContext || null,
		projectId,
		requestId: startSwitchRequest(key),
		hasCache,
		cachedHistoryCount,
		cacheRevisionAtRequest: getHistoryRevision(key),
	};
	startSessionRefresh(key, !hasCache);
	if (cachedHistory) {
		renderSessionHistory(key, cachedHistory, context.searchContext, null, cachedHistoryCount, false);
	} else {
		showSessionLoadIndicator();
	}

	const switchParams: SwitchRpcParams = { key, include_history: false };
	if (projectId) switchParams.project_id = projectId;
	void sendRpc<SwitchPayload>("sessions.switch", switchParams)
		.then((response) => handleSwitchResponse(context, response))
		.catch(() => {
			if (isLatestSwitchRequest(key, context.requestId)) {
				finishSwitchWithError(context, "Failed to load session");
			}
		});
}
