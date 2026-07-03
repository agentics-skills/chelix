// ── Chat send logic ──────────────────────────────────────────

import { chatAddMsg, chatAddMsgWithAttachments, setComposerStopButton } from "../../chat-ui";
import { highlightCodeBlocks } from "../../code-highlight";
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
import type { SessionMeta } from "../../types";
import type { RpcResponse } from "../../types/rpc";
import { handleSlashCommand, parseSlashCommand, shouldHandleSlashLocally, slashHideMenu } from "./slash-commands";

// ── Types ────────────────────────────────────────────────────

export interface ChatSendParams {
	text?: string;
	content?: ChatContentPart[];
	_document_files?: UploadedDocumentFile[];
	_seq: number;
	model?: string;
}

export type ChatContentPart = { type: "text"; text: string } | { type: "image_url"; image_url: { url: string } };

interface PendingImageAttachment extends PendingAttachment {
	dataUrl: string;
}

export interface ChatSendPayload {
	runId?: string;
	queued?: boolean;
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
	if (!(slash && shouldHandleSlashLocally(slash.name, slash.args))) return false;
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
	localStorage.setItem("moltis-chat-history", JSON.stringify(S.chatHistory));
}

export function resetComposerAfterSend(): void {
	S.setChatHistoryIdx(-1);
	S.setChatHistoryDraft("");
	(S.chatInput as HTMLTextAreaElement).value = "";
	chatAutoResize();
	if (window.innerWidth < 768) S.chatInput?.blur();
}

export function normalizeOutgoingText(text: string, hasAttachments: boolean): string {
	if (!(S.commandModeEnabled && text && !hasAttachments)) return text;
	const parsed = parseSlashCommand(text);
	if (parsed && parsed.name === "sh") return text;
	return `/sh ${text}`;
}

export function applySelectedModelToChatParams(chatParams: ChatSendParams): void {
	const effectiveId = modelStore.effectiveModelId.value;
	if (!effectiveId) return;
	chatParams.model = effectiveId;
	setSessionModel(S.activeSessionKey, effectiveId);
}

export function handleChatSendRpcResponse(res: RpcResponse<ChatSendPayload>, userEl: HTMLElement | null): boolean {
	if (res.ok && res.payload?.runId) setSessionActiveRunId(S.activeSessionKey, res.payload.runId);
	if (res.payload?.queued) {
		markMessageQueued(userEl, S.activeSessionKey);
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
	displayText?: string,
): Promise<{ params: ChatSendParams; el: HTMLElement | null; enableDeleteAction: () => void }> {
	const userText = displayText !== undefined ? displayText : text;
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
		const el = chatAddMsgWithAttachments("user", userText ? renderMarkdown(userText) : "", images, uploadedDocuments);
		appendUserMessageActions({
			messageEl: el,
			sessionKey: S.activeSessionKey,
			text: userText,
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
					text: userText,
					seq,
					onDeleted: (payload) => handleUserMessageDeleted(el, payload),
				}),
		};
	}
	const el = chatAddMsg("user", renderMarkdown(userText), true);
	appendUserMessageActions({
		messageEl: el,
		sessionKey: S.activeSessionKey,
		text: userText,
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
				text: userText,
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

function rollbackOptimisticSend(snapshot: OptimisticSendSnapshot, userEl: HTMLElement | null): void {
	if (userEl?.isConnected) userEl.remove();
	S.setChatSeq(snapshot.previousChatSeq);

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
	setSessionReplying(snapshot.sessionKey, false);
	setComposerStopButton(false);
}

function markMessageQueued(el: HTMLElement | null, sessionKey: string): void {
	if (!el) return;
	const tray = document.getElementById("queuedMessages");
	if (!tray) return;
	console.debug("[queued] marking user message as queued, moving to tray", { sessionKey });
	el.classList.add("queued");
	const badge = document.createElement("div");
	badge.className = "queued-badge";
	const label = document.createElement("span");
	label.className = "queued-label";
	label.textContent = "Queued";
	const btn = document.createElement("button");
	btn.className = "queued-cancel";
	btn.title = "Cancel all queued";
	btn.textContent = "\u2715";
	btn.addEventListener("click", (e: MouseEvent) => {
		e.stopPropagation();
		sendRpc("chat.cancel_queued", { sessionKey });
	});
	badge.appendChild(label);
	badge.appendChild(btn);
	el.appendChild(badge);
	tray.appendChild(el);
	tray.classList.remove("hidden");
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
		const outgoingText = normalizeOutgoingText(text, hasAttachments);
		const previousChatSeq = S.chatSeq;
		S.setChatSeq(previousChatSeq + 1);
		const msg = await buildChatMessage(outgoingText, S.chatSeq, text);
		const rollbackSnapshot = captureOptimisticSendSnapshot(S.activeSessionKey, previousChatSeq);
		rememberChatHistory(text);
		resetComposerAfterSend();
		const chatParams = msg.params;
		const userEl = msg.el;
		if (userEl) highlightCodeBlocks(userEl);
		applySelectedModelToChatParams(chatParams);
		bumpSessionCount(S.activeSessionKey, 1);
		cacheOutgoingUserMessage(S.activeSessionKey, chatParams);
		seedSessionPreviewFromUserText(S.activeSessionKey, text || outgoingText);
		setSessionReplying(S.activeSessionKey, true);
		setComposerStopButton(true, S.activeSessionKey);
		try {
			const res = await sendRpc<ChatSendPayload>("chat.send", chatParams);
			const accepted = handleChatSendRpcResponse(res, userEl);
			if (!accepted) {
				rollbackOptimisticSend(rollbackSnapshot, userEl);
			} else if (res.ok && !res.payload?.queued) {
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
