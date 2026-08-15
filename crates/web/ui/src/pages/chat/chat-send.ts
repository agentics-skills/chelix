// ── Chat send logic ──────────────────────────────────────────

import { chatAddMsg, chatAddMsgWithAttachments, setComposerStopButton } from "../../chat-ui";
import { highlightCodeBlocks } from "../../code-highlight";
import { unmountExecuteCommandToolBubbles } from "../../components/ExecuteCommandToolBubble";
import { renderMarkdown, sendRpc, warmAudioPlayback } from "../../helpers";
import {
	clearPendingAttachments,
	getPendingAttachments,
	hasPendingAttachments,
	type PendingAttachment,
	type UploadedDocumentFile,
	uploadDocumentAttachment,
} from "../../media-drop";
import { appendUserMessageActions } from "../../message-actions";
import { setSessionModel } from "../../models";
import {
	bumpSessionCount,
	cacheOutgoingUserMessage,
	clearSessionHistoryCache,
	markSessionTailLocallyTruncated,
	seedSessionPreviewFromUserText,
	setSessionActiveRunId,
	setSessionReplying,
} from "../../sessions";
import * as S from "../../state";
import { modelStore } from "../../stores/model-store";
import { sessionStore } from "../../stores/session-store";
import type { RpcResponse } from "../../types/rpc";
import type { SessionMeta } from "../../types/session";
import type { QueuedPrompt } from "../../types/ws-events";
import { setQueuedPrompts } from "./prompt-queue";
import { handleSlashCommand, parseSlashCommand, shouldHandleSlashLocally, slashHideMenu } from "./slash-commands";

// ── Types ────────────────────────────────────────────────────

export interface ChatSendParams {
	text?: string;
	content?: ChatContentPart[];
	_document_files?: UploadedDocumentFile[];
	_seq: number;
	model?: string;
	reasoningEffort?: string;
}

export type ChatContentPart = { type: "text"; text: string } | { type: "image_url"; image_url: { url: string } };

interface PendingImageAttachment extends PendingAttachment {
	dataUrl: string;
}

export interface ChatSendPayload {
	runId?: string;
	queued?: boolean;
	prompts?: QueuedPrompt[];
}

type TruncateTailEntry = Parameters<typeof markSessionTailLocallyTruncated>[2];

interface TruncateTailPayload {
	sessionKey?: string;
	keptCount?: number;
	entry?: TruncateTailEntry;
}

interface SessionOptimisticSnapshot {
	messageCount: number;
	lastSeenMessageCount: number;
	preview: string;
	updatedAt: number;
	lastHistoryIndex: number;
	version: number;
}

interface LegacySessionOptimisticSnapshot {
	messageCount?: number;
	lastSeenMessageCount?: number;
	preview?: string | null;
	updatedAt?: number;
	version?: number;
	_localUnread?: boolean;
	_replying?: boolean;
}

interface OptimisticSendSnapshot {
	sessionKey: string;
	previousChatSeq: number;
	session?: SessionOptimisticSnapshot;
	legacy?: LegacySessionOptimisticSnapshot;
}

// ── Auto-resize ─────────────────────────────────────────────

function chatAutoResize(): void {
	if (!S.chatInput) return;
	S.chatInput.style.height = "auto";
	S.chatInput.style.height = `${Math.min(S.chatInput.scrollHeight, 120)}px`;
}

// ── Slash command integration ───────────────────────────────

export function tryHandleLocalSlashCommand(text: string, hasAttachments: boolean): boolean {
	if (text.charAt(0) !== "/" || hasAttachments) return false;
	const slash = parseSlashCommand(text);
	if (!(slash && shouldHandleSlashLocally(slash.name))) return false;
	(S.chatInput as HTMLTextAreaElement).value = "";
	chatAutoResize();
	slashHideMenu();
	handleSlashCommand(slash.name, slash.args);
	return true;
}

// ── History navigation ──────────────────────────────────────

export function handleHistoryUp(): void {
	if (S.chatHistory.length === 0) return;
	if (S.chatHistoryIdx === -1) {
		S.setChatHistoryDraft((S.chatInput as HTMLTextAreaElement).value);
		S.setChatHistoryIdx(S.chatHistory.length - 1);
	} else if (S.chatHistoryIdx > 0) S.setChatHistoryIdx(S.chatHistoryIdx - 1);
	(S.chatInput as HTMLTextAreaElement).value = S.chatHistory[S.chatHistoryIdx];
	chatAutoResize();
}

export function handleHistoryDown(): void {
	if (S.chatHistoryIdx === -1) return;
	if (S.chatHistoryIdx < S.chatHistory.length - 1) {
		S.setChatHistoryIdx(S.chatHistoryIdx + 1);
		(S.chatInput as HTMLTextAreaElement).value = S.chatHistory[S.chatHistoryIdx];
	} else {
		S.setChatHistoryIdx(-1);
		(S.chatInput as HTMLTextAreaElement).value = S.chatHistoryDraft;
	}
	chatAutoResize();
}

// ── Send helpers ────────────────────────────────────────────

export function rememberChatHistory(text: string): void {
	if (!text) return;
	S.chatHistory.push(text);
	if (S.chatHistory.length > 200) S.setChatHistory(S.chatHistory.slice(-200));
	localStorage.setItem("chelix-chat-history", JSON.stringify(S.chatHistory));
}

export function resetComposerAfterSend(): void {
	S.setChatHistoryIdx(-1);
	S.setChatHistoryDraft("");
	(S.chatInput as HTMLTextAreaElement).value = "";
	chatAutoResize();
	if (window.innerWidth < 768) S.chatInput?.blur();
}

export function applySelectedModelToChatParams(chatParams: ChatSendParams): void {
	const modelId = modelStore.selectedModelId.value;
	if (!modelId) return;
	const reasoningEffort = modelStore.supportsReasoning.value ? modelStore.reasoningEffort.value : "";
	chatParams.model = modelId;
	chatParams.reasoningEffort = reasoningEffort;
	setSessionModel(S.activeSessionKey, modelId, reasoningEffort);
}

export function handleChatSendRpcResponse(res: RpcResponse<ChatSendPayload>, userEl: HTMLElement | null): boolean {
	if (res.ok && res.payload?.runId) setSessionActiveRunId(S.activeSessionKey, res.payload.runId);
	if (res.payload?.queued) {
		// The prompt is now server state; the optimistic bubble is replaced by
		// the queue tray, which every client renders from the same snapshot.
		userEl?.remove();
		setQueuedPrompts(S.activeSessionKey, res.payload.prompts ?? []);
		return true;
	}
	if (!res.ok) {
		setComposerStopButton(false);
		chatAddMsg("error", res.error?.message || "Request failed");
		return false;
	}
	return res.ok;
}

export async function buildChatMessage(
	text: string,
	seq: number,
): Promise<{ params: ChatSendParams; el: HTMLElement | null; enableDeleteAction: () => void }> {
	const attachments = hasPendingAttachments() ? getPendingAttachments() : [];
	const images = attachments.filter((attachment): attachment is PendingImageAttachment => Boolean(attachment.dataUrl));
	const documents = attachments.filter((attachment) => !attachment.dataUrl);
	if (attachments.length > 0) {
		const uploadedDocuments = await Promise.all(
			documents.map((attachment) => uploadDocumentAttachment(attachment, S.activeSessionKey)),
		);
		const content: ChatContentPart[] = [];
		if (text) content.push({ type: "text", text });
		for (const img of images) if (img.dataUrl) content.push({ type: "image_url", image_url: { url: img.dataUrl } });
		const params: ChatSendParams = content.length > 0 ? { content, _seq: seq } : { text, _seq: seq };
		if (uploadedDocuments.length > 0) params._document_files = uploadedDocuments;
		const el = chatAddMsgWithAttachments("user", text ? renderMarkdown(text) : "", images, uploadedDocuments);
		appendUserMessageActions({
			messageEl: el,
			sessionKey: S.activeSessionKey,
			text,
			seq,
			deleteEnabled: false,
			onDeleted: (payload) => handleUserMessageDeleted(el, payload),
		});
		clearPendingAttachments();
		return {
			params,
			el,
			enableDeleteAction: () =>
				appendUserMessageActions({
					messageEl: el,
					sessionKey: S.activeSessionKey,
					text,
					seq,
					onDeleted: (payload) => handleUserMessageDeleted(el, payload),
				}),
		};
	}
	const el = chatAddMsg("user", renderMarkdown(text), true);
	appendUserMessageActions({
		messageEl: el,
		sessionKey: S.activeSessionKey,
		text,
		seq,
		deleteEnabled: false,
		onDeleted: (payload) => handleUserMessageDeleted(el, payload),
	});
	return {
		params: { text, _seq: seq },
		el,
		enableDeleteAction: () =>
			appendUserMessageActions({
				messageEl: el,
				sessionKey: S.activeSessionKey,
				text,
				seq,
				onDeleted: (payload) => handleUserMessageDeleted(el, payload),
			}),
	};
}

function handleUserMessageDeleted(messageEl: HTMLElement | null, payload: unknown): void {
	const data = payload as TruncateTailPayload | null;
	const sessionKey = data?.sessionKey || S.activeSessionKey;
	markSessionTailLocallyTruncated(sessionKey, Number(data?.keptCount) || 0, data?.entry);
	if (sessionKey !== S.activeSessionKey || !location.pathname.startsWith("/chats/")) return;
	removeMessageTailFromDom(messageEl);
}

function removeMessageTailFromDom(messageEl: HTMLElement | null): void {
	let current = messageEl;
	while (current) {
		const next = current.nextElementSibling as HTMLElement | null;
		unmountExecuteCommandToolBubbles(current);
		current.remove();
		current = next;
	}
}

function captureOptimisticSendSnapshot(sessionKey: string, previousChatSeq: number): OptimisticSendSnapshot {
	const session = sessionStore.getByKey(sessionKey);
	const legacy = (S.sessions as SessionMeta[]).find((entry) => entry.key === sessionKey);
	return {
		sessionKey,
		previousChatSeq,
		session: session
			? {
					messageCount: session.messageCount,
					lastSeenMessageCount: session.lastSeenMessageCount,
					preview: session.preview,
					updatedAt: session.updatedAt,
					lastHistoryIndex: session.lastHistoryIndex.value,
					version: session.version,
				}
			: undefined,
		legacy: legacy
			? {
					messageCount: legacy.messageCount,
					lastSeenMessageCount: legacy.lastSeenMessageCount,
					preview: legacy.preview,
					updatedAt: legacy.updatedAt,
					version: legacy.version,
					_localUnread: legacy._localUnread,
					_replying: legacy._replying,
				}
			: undefined,
	};
}

/** Undo the optimistic session bookkeeping applied before `chat.send`. */
function restoreOptimisticSessionState(snapshot: OptimisticSendSnapshot, userEl: HTMLElement | null): void {
	if (userEl?.isConnected) userEl.remove();

	const session = sessionStore.getByKey(snapshot.sessionKey);
	if (session && snapshot.session) {
		session.messageCount = snapshot.session.messageCount;
		session.lastSeenMessageCount = snapshot.session.lastSeenMessageCount;
		session.preview = snapshot.session.preview;
		session.updatedAt = snapshot.session.updatedAt;
		session.lastHistoryIndex.value = snapshot.session.lastHistoryIndex;
		session.version = snapshot.session.version;
		session.updateBadge();
		session.dataVersion.value++;
	}

	const legacy = (S.sessions as SessionMeta[]).find((entry) => entry.key === snapshot.sessionKey);
	if (legacy && snapshot.legacy) {
		legacy.messageCount = snapshot.legacy.messageCount;
		legacy.lastSeenMessageCount = snapshot.legacy.lastSeenMessageCount;
		legacy.preview = snapshot.legacy.preview;
		legacy.updatedAt = snapshot.legacy.updatedAt;
		legacy.version = snapshot.legacy.version;
		legacy._localUnread = snapshot.legacy._localUnread;
		legacy._replying = snapshot.legacy._replying;
	}

	clearSessionHistoryCache(snapshot.sessionKey);
}

/** Roll back a send the server rejected: the session is idle again. */
function rollbackOptimisticSend(snapshot: OptimisticSendSnapshot, userEl: HTMLElement | null): void {
	restoreOptimisticSessionState(snapshot, userEl);
	// The server stored nothing, so this seq is free again.
	S.setChatSeq(snapshot.previousChatSeq);
	setSessionReplying(snapshot.sessionKey, false);
	setComposerStopButton(false);
}

/**
 * Roll back a send the server queued. The prompt is not part of the
 * conversation yet — it is rendered from the server queue snapshot — but the
 * active run keeps the session busy.
 *
 * `chatSeq` is deliberately kept: the queued prompt carries this seq on the
 * server and is persisted with it when the queue is replayed. Reusing the seq
 * for the next message would produce two user messages sharing one seq, which
 * breaks echo suppression and makes deletion truncate at the wrong message.
 */
function rollbackQueuedSend(snapshot: OptimisticSendSnapshot, userEl: HTMLElement | null): void {
	restoreOptimisticSessionState(snapshot, userEl);
}

// ── Main sendChat function ──────────────────────────────────
// Exposed so ChatPage and slash-commands can call it.

let maybeRefreshFullContextFn: (() => void) | null = null;

/** Called by ChatPage to register the refresh callback. */
export function setMaybeRefreshFullContextFn(fn: () => void): void {
	maybeRefreshFullContextFn = fn;
}

let sendInProgress = false;

export function sendChat(): void {
	void sendChatAsync();
}

async function sendChatAsync(): Promise<void> {
	if (sendInProgress) return;
	const text = (S.chatInput as HTMLTextAreaElement).value.trim();
	const hasAttachments = hasPendingAttachments();
	if (!((text || hasAttachments) && S.connected)) return;
	sendInProgress = true;
	warmAudioPlayback();
	try {
		if (tryHandleLocalSlashCommand(text, hasAttachments)) return;
		const previousChatSeq = S.chatSeq;
		S.setChatSeq(previousChatSeq + 1);
		const msg = await buildChatMessage(text, S.chatSeq);
		const rollbackSnapshot = captureOptimisticSendSnapshot(S.activeSessionKey, previousChatSeq);
		rememberChatHistory(text);
		resetComposerAfterSend();
		const chatParams = msg.params;
		const userEl = msg.el;
		if (userEl) highlightCodeBlocks(userEl);
		applySelectedModelToChatParams(chatParams);
		bumpSessionCount(S.activeSessionKey, 1);
		cacheOutgoingUserMessage(S.activeSessionKey, chatParams);
		seedSessionPreviewFromUserText(S.activeSessionKey, text);
		setSessionReplying(S.activeSessionKey, true);
		setComposerStopButton(true, S.activeSessionKey);
		try {
			const res = await sendRpc<ChatSendPayload>("chat.send", chatParams);
			const accepted = handleChatSendRpcResponse(res, userEl);
			if (!accepted) {
				rollbackOptimisticSend(rollbackSnapshot, userEl);
			} else if (res.payload?.queued) {
				rollbackQueuedSend(rollbackSnapshot, userEl);
			} else {
				msg.enableDeleteAction();
			}
		} catch {
			rollbackOptimisticSend(rollbackSnapshot, userEl);
			chatAddMsg("error", "Request failed");
		}
		maybeRefreshFullContextFn?.();
	} catch (err) {
		chatAddMsg("error", err instanceof Error ? err.message : "File upload failed");
	} finally {
		sendInProgress = false;
	}
}

export { chatAutoResize };
