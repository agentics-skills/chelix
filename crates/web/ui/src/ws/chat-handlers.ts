// ── Chat event handler functions ──────────────────────────────

import {
	appendReasoningDisclosure,
	chatAddErrorCard,
	chatAddErrorMsg,
	chatAddMsg,
	removeThinking,
	setComposerStopButton,
	smartScrollToBottom,
	updateTokenBar,
} from "../chat-ui";
import { highlightCodeBlocks } from "../code-highlight";
import { localizeStructuredError, renderAudioPlayer, renderMarkdown } from "../helpers";
import { t } from "../i18n";
import { appendMessageActions } from "../message-actions";
import { maybeRefreshFullContext } from "../pages/ChatPage";
import { renderCheckpointCard } from "../pages/chat/context-card";
import { currentPrefix } from "../router";
import {
	bumpSessionCount,
	cacheSessionHistoryMessage,
	clearSessionHistoryCache,
	fetchSessions,
	markSessionLocallyCleared,
	setSessionActiveRunId,
	setSessionReplying,
	setSessionUnread,
} from "../sessions";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import { terminalContextTokens } from "../terminal-usage";
import { resolveAssistantTurnEnd, toolCallIds } from "../tool-call-card";
import type { HistoryMessage } from "../types/session";
import type { AbortedPartialState, ChatPayload, ToolCallPayload } from "../types/ws-events";
import {
	clearChatEmptyState,
	hasNonWhitespaceContent,
	isReasoningAlreadyShown,
	makeThinkingDots,
	moveFirstQueuedToChat,
	setSafeMarkdownHtml,
	updateSessionHistoryIndex,
	updateSessionRunId,
} from "./shared";
import {
	clearPendingToolCallEndsForSession,
	clearStaleRunningToolCards,
	closeLiveAssistantSegment,
	completeToolCard,
	createToolCallCardForPayload,
	handleToolCallStartDom,
	pendingToolCallEnds,
	renderAbortedPartialInDom,
	renderChannelUserMessage,
	resolveFinalMessageEl,
	toolCallCardId,
	toolCallEventKey,
} from "./tool-helpers";

export type ChatHandler = (p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string) => void;

// ── Individual chat event handlers ────────────────────────────

function sessionMediaUrl(sessionKey: string, audioPath: string): string | null {
	const filename = audioPath.split("/").pop() || "";
	if (!filename) return null;
	return `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(filename)}`;
}

function assistantHistoryMessage(message: NonNullable<ToolCallPayload["assistantMessage"]>): HistoryMessage {
	return {
		role: message.role,
		content: message.content,
		model: message.model,
		provider: message.provider,
		reasoningEffort: message.reasoningEffort,
		inputTokens: message.inputTokens,
		outputTokens: message.outputTokens,
		cacheReadTokens: message.cacheReadTokens,
		cacheWriteTokens: message.cacheWriteTokens,
		durationMs: message.durationMs,
		requestInputTokens: message.requestInputTokens,
		requestOutputTokens: message.requestOutputTokens,
		requestCacheReadTokens: message.requestCacheReadTokens,
		requestCacheWriteTokens: message.requestCacheWriteTokens,
		tool_calls: message.tool_calls,
		reasoning: message.reasoning,
		audio: message.audio,
		run_id: message.run_id,
		created_at: message.created_at,
		seq: message.seq,
	};
}

function handleChatThinking(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	removeThinking();
	clearChatEmptyState();
	const thinkEl = document.createElement("div");
	thinkEl.className = "msg assistant thinking";
	thinkEl.id = "thinkingIndicator";
	thinkEl.appendChild(makeThinkingDots());
	S.chatMsgBox?.appendChild(thinkEl);
	smartScrollToBottom();
}

function handleChatThinkingText(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	const indicator = document.getElementById("thinkingIndicator");
	if (indicator) {
		while (indicator.firstChild) indicator.removeChild(indicator.firstChild);
		const textEl = document.createElement("span");
		textEl.className = "thinking-text";
		textEl.textContent = p.text || "";
		indicator.appendChild(textEl);
		smartScrollToBottom();
	}
}

function handleChatThinkingDone(_p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	// Don't remove the thinking indicator here. It will be removed by either:
	// - handleChatDelta (when text starts streaming)
	// - handleChatToolCallStart (which preserves thinking text as a disclosure)
	// - handleChatFinal / handleChatError (cleanup)
	// This keeps the thinking text visible until we know whether to preserve it.
	void (isActive && isChatPage);
}

function handleChatVoicePending(_p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	// Update per-session signal
	const session = sessionStore.getByKey(eventSession);
	if (session) session.voicePending.value = true;
	if (!(isActive && isChatPage)) return;
	// Dual-write to global state for backward compat
	S.setVoicePending(true);
	// Keep the existing thinking dots visible -- no separate voice indicator.
}

function handleChatToolCallStart(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	const toolSession = sessionStore.getByKey(eventSession);
	const knownIndex = toolSession ? toolSession.lastHistoryIndex.value : S.lastHistoryIndex;
	const messageIndex = p.messageIndex;
	if (p.assistantMessage && typeof messageIndex === "number" && Number.isInteger(messageIndex)) {
		if (messageIndex > knownIndex) bumpSessionCount(eventSession, 1);
		cacheSessionHistoryMessage(eventSession, assistantHistoryMessage(p.assistantMessage), messageIndex);
		updateSessionHistoryIndex(eventSession, messageIndex);
	}
	// Update per-session signal
	if (toolSession) toolSession.streamText.value = "";
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	handleToolCallStartDom(p, eventSession);
}

function updateActiveTokenBarContextBudget(p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	if (isActive && isChatPage && p.contextBudget) updateTokenBar(p.contextBudget);
}

function cacheRejectedAssistantFrame(p: ChatPayload, eventSession: string): void {
	if (p.rejected !== true || !p.assistantMessage || !Number.isInteger(p.assistantMessageIndex)) return;
	const assistantIndex = p.assistantMessageIndex as number;
	const session = sessionStore.getByKey(eventSession);
	const knownIndex = session ? session.lastHistoryIndex.value : S.lastHistoryIndex;
	if (assistantIndex > knownIndex) bumpSessionCount(eventSession, 1);
	cacheSessionHistoryMessage(eventSession, assistantHistoryMessage(p.assistantMessage), assistantIndex);
	updateSessionHistoryIndex(eventSession, assistantIndex);
}

function toolResultHistoryIndex(p: ChatPayload, eventSession: string): number | undefined {
	if (p.messageIndex !== undefined && p.messageIndex !== null) return p.messageIndex;
	const session = sessionStore.getByKey(eventSession);
	return session && session.messageCount > 0 ? session.messageCount - 1 : undefined;
}

function toolResultError(p: ChatPayload): string | null {
	if (typeof p.error === "string") return p.error;
	return p.error?.detail || p.error?.message || null;
}

function cacheToolResultFrame(p: ChatPayload, eventSession: string): void {
	const historyIndex = toolResultHistoryIndex(p, eventSession);
	const session = sessionStore.getByKey(eventSession);
	const knownIndex = session ? session.lastHistoryIndex.value : S.lastHistoryIndex;
	if (historyIndex === undefined || historyIndex > knownIndex) bumpSessionCount(eventSession, 1);
	cacheSessionHistoryMessage(
		eventSession,
		{
			role: "tool_result",
			tool_call_id: p.toolCallId || "",
			tool_name: p.toolName || "",
			arguments: p.arguments,
			success: p.success === true,
			rejected: p.rejected === true,
			result: p.result || null,
			error: toolResultError(p),
			contextBudget: p.contextBudget,
			created_at: Date.now(),
		},
		historyIndex,
	);
	updateSessionHistoryIndex(eventSession, historyIndex);
}

function renderToolCallEnd(p: ChatPayload, eventSession: string): void {
	// A rejected call has no `tool_call_start`, so nothing has closed the live
	// assistant segment yet.
	if (p.rejected === true) {
		removeThinking();
		closeLiveAssistantSegment(p.assistantMessage, p.assistantMessageIndex, eventSession);
	}
	const toolCard =
		(document.getElementById(toolCallCardId(p)) as HTMLElement | null) || createToolCallCardForPayload(p);
	if (!toolCard) {
		pendingToolCallEnds.set(toolCallEventKey(eventSession, p), p as ToolCallPayload);
		return;
	}
	completeToolCard(toolCard, p, eventSession);
}

function handleChatToolCallEnd(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	cacheRejectedAssistantFrame(p, eventSession);
	cacheToolResultFrame(p, eventSession);
	updateActiveTokenBarContextBudget(p, isActive, isChatPage);
	if (isActive && isChatPage) renderToolCallEnd(p, eventSession);
}

function handleChatChannelUser(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	// Always bump the badge so the total message count stays accurate,
	// even when the user is not on the chat page (e.g. Telegram messages).
	bumpSessionCount(eventSession, 1);
	const cachedAudio = p.channel?.audio_filename
		? `media/${eventSession.replaceAll(":", "_")}/${p.channel.audio_filename}`
		: undefined;
	cacheSessionHistoryMessage(
		eventSession,
		{
			role: "user",
			content: p.text || "",
			channel: p.channel || null,
			audio: cachedAudio,
			created_at: Date.now(),
		},
		p.messageIndex,
	);
	if (!isActive) {
		setSessionUnread(eventSession, true);
	}
	if (!(isChatPage && isActive)) {
		updateSessionHistoryIndex(eventSession, p.messageIndex);
		return;
	}
	renderChannelUserMessage(p, eventSession);
	updateSessionHistoryIndex(eventSession, p.messageIndex);
}

// Handle user messages broadcast by the backend after persisting a message
// sent via the GraphQL API, mobile app, or any non-web-UI client.
// The originating web client already rendered the message optimistically,
// so we skip rendering when the broadcast's seq matches a seq this client
// has already sent (seq <= S.chatSeq).
function handleChatUserMessage(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	// Suppress the echo for the originating client.
	if (p.seq !== undefined && p.seq !== null && p.seq <= S.chatSeq) return;
	const msgSession = sessionStore.getByKey(eventSession);
	const lastIdx = msgSession ? msgSession.lastHistoryIndex.value : -1;
	if (p.messageIndex !== undefined && p.messageIndex !== null && p.messageIndex <= lastIdx) return;

	bumpSessionCount(eventSession, 1);
	cacheSessionHistoryMessage(
		eventSession,
		{
			role: "user",
			content: p.text || "",
			created_at: Date.now(),
		},
		p.messageIndex,
	);
	if (!isActive) {
		setSessionUnread(eventSession, true);
	}
	updateSessionHistoryIndex(eventSession, p.messageIndex);
	if (!(isChatPage && isActive)) return;
	// Safe: renderMarkdown calls esc() first -- all user input is
	// HTML-escaped before formatting tags are applied.
	chatAddMsg("user", renderMarkdown(p.text || ""), true);
}

function handleChatDelta(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	if (!p.text) return;
	// Update per-session signal
	const session = sessionStore.getByKey(eventSession);
	if (session) session.streamText.value += p.text;
	if (!(isActive && isChatPage)) return;
	// When voice is pending, accumulate text silently without rendering.
	if (S.voicePending) {
		S.setStreamText(S.streamText + p.text);
		return;
	}
	// Skip leading whitespace before any real content has been streamed.
	// Some providers emit newlines between thinking and content; rendering
	// them would create an empty assistant div that lingers if a tool call
	// follows immediately.  We must check this BEFORE removeThinking() so
	// the thinking text is still available for handleChatToolCallStart to
	// extract into a reasoning disclosure on the tool card.
	if (!(S.streamEl || p.text.trim())) return;
	removeThinking();
	let streamElement = S.streamEl;
	if (!streamElement) {
		S.setStreamText("");
		streamElement = document.createElement("div");
		streamElement.className = "msg assistant";
		S.setStreamEl(streamElement);
		clearChatEmptyState();
		S.chatMsgBox?.appendChild(streamElement);
	}
	S.setStreamText(S.streamText + p.text);
	setSafeMarkdownHtml(streamElement, S.streamText);
	if (Number.isInteger(p.messageIndex)) {
		streamElement.dataset.historyIndex = String(p.messageIndex);
	}
	appendMessageActions({
		messageEl: streamElement,
		sessionKey: eventSession,
		messageIndex: p.messageIndex,
		text: S.streamText,
	});
	smartScrollToBottom();
}

interface FinalMessageContext {
	text: string;
	hasVisibleContent: boolean;
}

function finalMessageContext(payload: ChatPayload): FinalMessageContext {
	const text = String(payload.text || "");
	return {
		text,
		hasVisibleContent:
			hasNonWhitespaceContent(text) ||
			hasNonWhitespaceContent(payload.reasoning || "") ||
			hasNonWhitespaceContent(payload.audio || ""),
	};
}

function finalAssistantHistory(payload: ChatPayload, text: string): HistoryMessage {
	return {
		role: "assistant",
		content: text,
		model: payload.model || "",
		provider: payload.provider || "",
		inputTokens: payload.inputTokens || 0,
		outputTokens: payload.outputTokens || 0,
		cacheReadTokens: payload.cacheReadTokens || 0,
		cacheWriteTokens: payload.cacheWriteTokens || 0,
		durationMs: payload.durationMs || 0,
		requestInputTokens: payload.requestInputTokens,
		requestOutputTokens: payload.requestOutputTokens,
		requestCacheReadTokens: payload.requestCacheReadTokens,
		requestCacheWriteTokens: payload.requestCacheWriteTokens,
		reasoningEffort: payload.reasoningEffort,
		reasoning: payload.reasoning || undefined,
		audio: payload.audio || undefined,
		run_id: payload.runId || undefined,
		created_at: Date.now(),
	};
}

function cacheFinalMessage(payload: ChatPayload, eventSession: string, context: FinalMessageContext): void {
	const session = sessionStore.getByKey(eventSession);
	const lastHistoryIndex = session ? session.lastHistoryIndex.value : S.lastHistoryIndex;
	if (payload.messageIndex === undefined || payload.messageIndex > lastHistoryIndex) bumpSessionCount(eventSession, 1);
	if (context.hasVisibleContent) {
		cacheSessionHistoryMessage(eventSession, finalAssistantHistory(payload, context.text), payload.messageIndex);
	}
	updateSessionHistoryIndex(eventSession, payload.messageIndex);
	setSessionReplying(eventSession, false);
	setSessionActiveRunId(eventSession, null);
}

function prepareFinalMessageDom(): void {
	setComposerStopButton(false);
	removeThinking();
	clearStaleRunningToolCards();
}

function appendFinalText(messageElement: HTMLElement, text: string): void {
	if (!hasNonWhitespaceContent(text)) return;
	const textWrap = document.createElement("div");
	textWrap.className = "mt-2";
	setSafeMarkdownHtml(textWrap, text);
	messageElement.appendChild(textWrap);
}

function appendFinalReasoning(messageElement: HTMLElement, reasoning: string | undefined): void {
	if (reasoning && !isReasoningAlreadyShown(reasoning)) appendReasoningDisclosure(messageElement, reasoning);
}

function renderVoicePendingFinal(payload: ChatPayload, text: string): HTMLElement {
	console.debug("[audio] voice-pending path, audio:", Boolean(payload.audio), "text:", text.substring(0, 40));
	const messageElement = S.streamEl || document.createElement("div");
	messageElement.className = "msg assistant";
	messageElement.textContent = "";
	if (!messageElement.parentNode) {
		clearChatEmptyState();
		S.chatMsgBox?.appendChild(messageElement);
	}
	if (payload.audio) {
		const audioSource = sessionMediaUrl(payload.sessionKey || S.activeSessionKey, payload.audio);
		if (audioSource) {
			console.debug("[audio] rendering persisted audio:", payload.audio);
			renderAudioPlayer(messageElement, audioSource, true);
		}
	}
	appendFinalText(messageElement, text);
	appendFinalReasoning(messageElement, payload.reasoning);
	smartScrollToBottom();
	return messageElement;
}

function renderStreamedVoiceAudio(payload: ChatPayload, text: string, messageElement: HTMLElement): void {
	console.debug(
		"[audio] streamed path, audio:",
		Boolean(payload.audio),
		"voicePending:",
		S.voicePending,
		"text:",
		text.substring(0, 40),
	);
	const audioSource = sessionMediaUrl(payload.sessionKey || S.activeSessionKey, payload.audio || "");
	console.debug("[audio] rendering persisted audio (streamed):", payload.audio);
	messageElement.textContent = "";
	if (audioSource) renderAudioPlayer(messageElement, audioSource, true);
	appendFinalText(messageElement, text);
}

function renderStreamedFinal(payload: ChatPayload, text: string): HTMLElement | null {
	let messageElement = resolveFinalMessageEl(payload);
	const reasoningAlreadyShown = Boolean(payload.reasoning && isReasoningAlreadyShown(payload.reasoning));
	if (!messageElement && payload.reasoning && !reasoningAlreadyShown) {
		messageElement = chatAddMsg("assistant", "", false);
	}
	if (messageElement && payload.reasoning && !reasoningAlreadyShown) {
		appendReasoningDisclosure(messageElement, payload.reasoning);
	}
	if (payload.replyMedium === "voice" && payload.audio) {
		messageElement ||= chatAddMsg("assistant", "", false);
		if (messageElement) renderStreamedVoiceAudio(payload, text, messageElement);
	}
	return messageElement;
}

function renderFinalMessage(payload: ChatPayload, text: string): HTMLElement | null {
	return S.voicePending && payload.replyMedium === "voice"
		? renderVoicePendingFinal(payload, text)
		: renderStreamedFinal(payload, text);
}

function appendFinalMessageActions(
	messageElement: HTMLElement | null,
	payload: ChatPayload,
	eventSession: string,
	text: string,
): void {
	if (!messageElement) return;
	appendMessageActions({
		messageEl: messageElement,
		sessionKey: eventSession,
		messageIndex: payload.messageIndex,
		text,
		hasAudio: Boolean(payload.audio),
	});
}

function updateFinalTokenState(payload: ChatPayload): void {
	if (payload.inputTokens || payload.outputTokens) {
		S.sessionTokens.input += payload.inputTokens || 0;
		S.sessionTokens.output += payload.outputTokens || 0;
	}
	if (payload.requestInputTokens !== undefined && payload.requestInputTokens !== null) {
		S.setSessionCurrentInputTokens(payload.requestInputTokens || 0);
	} else if (payload.inputTokens || payload.outputTokens) {
		S.setSessionCurrentInputTokens(payload.inputTokens || 0);
	}
	S.setSessionCurrentContextTokens(terminalContextTokens(payload));
	updateTokenBar();
}

function appendFinalTerminalMetadata(payload: ChatPayload, messageElement: HTMLElement | null): void {
	appendTerminalMetadata(
		S.chatMsgBox,
		resolveAssistantTurnEnd(payload.messageIndex, messageElement),
		terminalMetadataData(payload, {
			historyIndex: payload.messageIndex,
			runId: payload.runId,
			timestamp: Date.now(),
		}),
	);
}

function resetFinalStreamState(eventSession: string): void {
	sessionStore.getByKey(eventSession)?.resetStreamState();
	S.setStreamEl(null);
	S.setStreamText("");
	S.setLastToolOutput("");
	S.setVoicePending(false);
}

function finishFinalMessageUi(): void {
	maybeRefreshFullContext();
	if (S.chatMsgBox?.lastElementChild) highlightCodeBlocks(S.chatMsgBox.lastElementChild as HTMLElement);
	moveFirstQueuedToChat();
}

function handleChatFinal(payload: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearPendingToolCallEndsForSession(eventSession);
	updateSessionRunId(eventSession, payload.runId);
	const context = finalMessageContext(payload);
	cacheFinalMessage(payload, eventSession, context);
	if (!isActive) setSessionUnread(eventSession, true);
	if (!(isActive && isChatPage)) {
		S.setVoicePending(false);
		return;
	}
	prepareFinalMessageDom();
	const messageElement = renderFinalMessage(payload, context.text);
	appendFinalMessageActions(messageElement, payload, eventSession, context.text);
	updateFinalTokenState(payload);
	appendFinalTerminalMetadata(payload, messageElement);
	resetFinalStreamState(eventSession);
	finishFinalMessageUi();
}

// ── Compact handling ──────────────────────────────────────────

// Per-session reference to the "Summarizing conversation..." status
// message appended on `auto_compact start`. Tracked explicitly (not via
// `lastChild`) so removing it never touches the checkpoint card.
const compactingStatusElements: Map<string, HTMLElement> = new Map();

// Drop the "Summarizing conversation..." status message the auto-compact
// start phase appended for this session, if one exists.
function removeCompactingStatus(p: ChatPayload): void {
	const key = p.sessionKey || "__active__";
	const el = compactingStatusElements.get(key);
	compactingStatusElements.delete(key);
	if (el && el.parentNode === S.chatMsgBox) {
		S.chatMsgBox?.removeChild(el);
	}
}

function resetTokensAfterCompaction(): void {
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	updateTokenBar();
}

// Cache the persisted checkpoint message and render the checkpoint card.
// The card is rendered from the same persisted message that history
// rendering uses, so the live-stream and history paths look identical.
// A `data-history-index` guard makes duplicate broadcasts (`chat.compact
// done` + `auto_compact done` for the same checkpoint) idempotent.
function renderCheckpointFromPayload(
	p: ChatPayload,
	isActive: boolean,
	isChatPage: boolean,
	eventSession: string,
): void {
	const checkpoint = p.checkpoint;
	if (!checkpoint) return;
	const messageIndex = p.messageIndex;
	if (typeof messageIndex === "number" && Number.isInteger(messageIndex)) {
		const session = sessionStore.getByKey(eventSession);
		const knownIndex = session ? session.lastHistoryIndex.value : S.lastHistoryIndex;
		if (messageIndex > knownIndex) bumpSessionCount(eventSession, 1);
		cacheSessionHistoryMessage(eventSession, checkpoint as HistoryMessage, messageIndex);
		updateSessionHistoryIndex(eventSession, messageIndex);
	}
	if (!(isActive && isChatPage)) return;
	const cardIndex = typeof messageIndex === "number" ? String(messageIndex) : null;
	if (cardIndex && S.chatMsgBox?.querySelector(`.checkpoint-card[data-history-index="${cardIndex}"]`)) {
		return;
	}
	const card = renderCheckpointCard(checkpoint);
	if (card && cardIndex) card.dataset.historyIndex = cardIndex;
	smartScrollToBottom();
	resetTokensAfterCompaction();
}

function handleAutoCompactStart(p: ChatPayload, activePage: boolean): void {
	if (!activePage) return;
	const statusElement = chatAddMsg("system", "Summarizing conversation (context limit reached)\u2026");
	if (statusElement) compactingStatusElements.set(p.sessionKey || "__active__", statusElement);
}

function handleAutoCompactDone(p: ChatPayload, activePage: boolean, eventSession: string): void {
	if (activePage) removeCompactingStatus(p);
	renderCheckpointFromPayload(p, activePage, activePage, eventSession);
}

function handleAutoCompactError(p: ChatPayload, activePage: boolean): void {
	if (!activePage) return;
	removeCompactingStatus(p);
	chatAddMsg("error", `Auto-compact failed: ${p.error?.message || p.error?.detail || "unknown error"}`);
}

function handleChatAutoCompact(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	const activePage = isActive && isChatPage;
	if (p.phase === "start") handleAutoCompactStart(p, activePage);
	if (p.phase === "done") handleAutoCompactDone(p, activePage, eventSession);
	if (p.phase === "error") handleAutoCompactError(p, activePage);
}

// `chat.compact done` is emitted by ChatService::compact on every
// summarization (manual `/compact` RPCs and agent-loop auto-compact
// path). It carries the persisted checkpoint message from
// CheckpointOutcome::broadcast_metadata().
function handleChatCompact(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	if (p.phase !== "done") return;
	if (isActive && isChatPage) removeCompactingStatus(p);
	renderCheckpointFromPayload(p, isActive, isChatPage, eventSession);
}

// ── Retry handling ────────────────────────────────────────────

function retryDelayMsFromPayload(p: ChatPayload): number {
	if (p.retryAfterMs !== undefined && p.retryAfterMs !== null) return Number(p.retryAfterMs) || 0;
	if (p.error?.retryAfterMs !== undefined && p.error?.retryAfterMs !== null) return Number(p.error.retryAfterMs) || 0;
	return 0;
}

function retryStatusText(p: ChatPayload): string {
	const retryMs = retryDelayMsFromPayload(p);
	const retrySecs = Math.max(1, Math.ceil(retryMs / 1000));
	const rateLimited = p.error?.type === "rate_limit_exceeded";
	return rateLimited
		? `Rate limited by provider, retrying in ${retrySecs}s\u2026`
		: `Temporary provider issue, retrying in ${retrySecs}s\u2026`;
}

function handleChatRetrying(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);

	let indicator = document.getElementById("thinkingIndicator");
	if (!indicator) {
		removeThinking();
		indicator = document.createElement("div");
		indicator.className = "msg assistant thinking";
		indicator.id = "thinkingIndicator";
		indicator.appendChild(makeThinkingDots());
		clearChatEmptyState();
		S.chatMsgBox?.appendChild(indicator);
	}

	while (indicator.firstChild) indicator.removeChild(indicator.firstChild);
	const textEl = document.createElement("span");
	textEl.className = "thinking-text";
	textEl.textContent = retryStatusText(p);
	indicator.appendChild(textEl);
	smartScrollToBottom();
}

// ── Error / abort / notice / clear ────────────────────────────

function handleChatError(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearPendingToolCallEndsForSession(eventSession);
	setSessionReplying(eventSession, false);
	setSessionActiveRunId(eventSession, null);
	const partialState = getAbortedPartialState(p);
	const errSession = sessionStore.getByKey(eventSession);
	cacheAbortedPartial(eventSession, p, errSession, partialState);
	if (errSession) errSession.resetStreamState();
	if ((partialState.hasVisiblePartial || partialState.hasTerminalToolBatch) && !isActive) {
		setSessionUnread(eventSession, true);
	}
	if (!(isActive && isChatPage)) {
		S.setVoicePending(false);
		return;
	}
	setComposerStopButton(false);
	removeThinking();
	clearStaleRunningToolCards();
	renderAbortedPartialInDom(p, partialState);
	if (p.error?.title) {
		chatAddErrorCard(localizeStructuredError(p.error) as Parameters<typeof chatAddErrorCard>[0]);
	} else {
		chatAddErrorMsg(p.message || "unknown");
	}
	// Add continue button for max_iterations_reached errors.
	if (p.error?.canContinue) {
		const lastCard = S.chatMsgBox?.querySelector(".error-card:last-child") as HTMLElement | null;
		if (lastCard) {
			const btn = document.createElement("button");
			btn.className = "provider-btn error-continue-btn";
			btn.textContent = t("errors:chat.continue", "Continue");
			btn.onclick = () => {
				btn.disabled = true;
				btn.textContent = t("errors:chat.continuing", "Continuing...");
				(S.chatInput as HTMLInputElement).value = t(
					"errors:chat.continueMessage",
					"Please continue where you left off.",
				);
				// Trigger send by clicking the chat send button (sendChat is local to ChatPage)
				S.chatSendBtn?.click();
			};
			const body = lastCard.querySelector(".error-body");
			if (body) body.appendChild(btn);
		}
	}
	S.setStreamEl(null);
	S.setStreamText("");
	S.setVoicePending(false);
	moveFirstQueuedToChat();
}

function getAbortedPartialState(p: ChatPayload): AbortedPartialState {
	const partial = p.partialMessage && typeof p.partialMessage === "object" ? p.partialMessage : null;
	const partialText = String(partial?.content || "");
	const partialReasoning = String(partial?.reasoning || "");
	return {
		partial,
		partialText,
		partialReasoning,
		hasVisiblePartial: hasNonWhitespaceContent(partialText) || hasNonWhitespaceContent(partialReasoning),
		hasTerminalToolBatch: partial?.durationMs !== undefined && toolCallIds(partial.tool_calls).length > 0,
	};
}

function abortedPartialHistoryMessage(p: ChatPayload, partialState: AbortedPartialState): HistoryMessage {
	const partial = partialState.partial;
	return {
		role: "assistant",
		content: partialState.partialText,
		model: partial?.model || "",
		provider: partial?.provider || "",
		inputTokens: partial?.inputTokens || 0,
		outputTokens: partial?.outputTokens || 0,
		cacheReadTokens: partial?.cacheReadTokens || 0,
		cacheWriteTokens: partial?.cacheWriteTokens || 0,
		durationMs: partial?.durationMs || 0,
		requestInputTokens: partial?.requestInputTokens,
		requestOutputTokens: partial?.requestOutputTokens,
		requestCacheReadTokens: partial?.requestCacheReadTokens,
		requestCacheWriteTokens: partial?.requestCacheWriteTokens,
		tool_calls: partial?.tool_calls,
		reasoningEffort: partial?.reasoningEffort,
		reasoning: partial?.reasoning || undefined,
		audio: partial?.audio || undefined,
		run_id: partial?.run_id || p.runId || undefined,
		created_at: partial?.created_at || Date.now(),
	};
}

function cacheAbortedPartial(
	eventSession: string,
	p: ChatPayload,
	abortSession: ReturnType<typeof sessionStore.getByKey>,
	partialState: AbortedPartialState,
): void {
	if (!(partialState.hasVisiblePartial || partialState.hasTerminalToolBatch)) return;
	const lastIndex = abortSession ? abortSession.lastHistoryIndex.value : S.lastHistoryIndex;
	if (p.messageIndex === undefined || p.messageIndex === null || p.messageIndex > lastIndex) {
		bumpSessionCount(eventSession, 1);
	}
	cacheSessionHistoryMessage(eventSession, abortedPartialHistoryMessage(p, partialState), p.messageIndex);
	updateSessionHistoryIndex(eventSession, p.messageIndex);
}

function hasAbortedPartial(partialState: AbortedPartialState): boolean {
	return partialState.hasVisiblePartial || partialState.hasTerminalToolBatch;
}

function updateAbortedTokenState(partialState: AbortedPartialState): void {
	const partial = partialState.partial;
	if (partial?.inputTokens || partial?.outputTokens) {
		S.sessionTokens.input += partial.inputTokens || 0;
		S.sessionTokens.output += partial.outputTokens || 0;
	}
	if (partial?.requestInputTokens !== undefined) {
		S.setSessionCurrentInputTokens(partial.requestInputTokens || 0);
	} else if (partial?.inputTokens || partial?.outputTokens) {
		S.setSessionCurrentInputTokens(partial.inputTokens || 0);
	}
	if (!hasAbortedPartial(partialState)) return;
	S.setSessionCurrentContextTokens(terminalContextTokens(partial || {}));
	updateTokenBar();
}

function finalizeActiveAbort(p: ChatPayload, partialState: AbortedPartialState): void {
	setComposerStopButton(false);
	removeThinking();
	clearStaleRunningToolCards();
	renderAbortedPartialInDom(p, partialState);
	S.setStreamEl(null);
	S.setStreamText("");
	S.setVoicePending(false);
	moveFirstQueuedToChat();
}

function handleChatAborted(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearPendingToolCallEndsForSession(eventSession);
	setSessionReplying(eventSession, false);
	setSessionActiveRunId(eventSession, null);
	const partialState = getAbortedPartialState(p);
	const abortSession = sessionStore.getByKey(eventSession);
	cacheAbortedPartial(eventSession, p, abortSession, partialState);
	abortSession?.resetStreamState();
	if (hasAbortedPartial(partialState) && !isActive) setSessionUnread(eventSession, true);
	updateAbortedTokenState(partialState);
	if (isActive && isChatPage) {
		finalizeActiveAbort(p, partialState);
		return;
	}
	S.setVoicePending(false);
}

function handleChatNotice(p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	if (!(isActive && isChatPage)) return;
	// Render titled notices as markdown so emphasis is visible.
	const msg = p.title ? `**${p.title}:** ${p.message}` : p.message || "";
	const noticeEl = p.title ? chatAddMsg("system", renderMarkdown(msg), true) : chatAddMsg("system", msg);
	if (!(noticeEl && p.title)) return;
	noticeEl.classList.add("system-notice");
	if (String(p.title).toLowerCase() !== "sandbox") return;
	noticeEl.classList.add("system-notice-sandbox");
	const normalizedMessage = String(p.message || "").toLowerCase();
	if (normalizedMessage.indexOf("enabled") !== -1) {
		noticeEl.classList.add("is-enabled");
	} else if (normalizedMessage.indexOf("disabled") !== -1) {
		noticeEl.classList.add("is-disabled");
	}
}

function handleChatQueueCleared(_p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	if (!(isActive && isChatPage)) return;
	const tray = document.getElementById("queuedMessages");
	if (tray) {
		const count = tray.querySelectorAll(".msg").length;
		console.debug("[queued] queue_cleared: removing all from tray", { count });
		while (tray.firstChild) tray.removeChild(tray.firstChild);
		tray.classList.add("hidden");
	}
}

function handleChatSessionCleared(_p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearPendingToolCallEndsForSession(eventSession);
	setSessionActiveRunId(eventSession, null);
	clearSessionHistoryCache(eventSession);
	// Reset badge, unread state, and history index for every client.
	markSessionLocallyCleared(eventSession);
	if (isActive) {
		S.setLastHistoryIndex(-1);
		S.setChatSeq(0);
	}
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(false);
	// Active viewer: clear the chat box and token bar.
	if (S.chatMsgBox) S.chatMsgBox.textContent = "";
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	updateTokenBar();
}

// ── Handler map and dispatcher ────────────────────────────────

export const chatHandlers: Record<string, ChatHandler> = {
	thinking: handleChatThinking,
	thinking_text: handleChatThinkingText,
	thinking_done: handleChatThinkingDone,
	voice_pending: handleChatVoicePending,
	tool_call_start: handleChatToolCallStart,
	tool_call_end: handleChatToolCallEnd,
	channel_user: handleChatChannelUser,
	user_message: handleChatUserMessage,
	delta: handleChatDelta,
	final: handleChatFinal,
	auto_compact: handleChatAutoCompact,
	compact: handleChatCompact,
	retrying: handleChatRetrying,
	error: handleChatError,
	aborted: handleChatAborted,
	notice: handleChatNotice,
	queue_cleared: handleChatQueueCleared,
	session_cleared: handleChatSessionCleared,
};

export function handleChatEvent(p: ChatPayload): void {
	const eventSession = p.sessionKey || sessionStore.activeSessionKey.value;
	const isActive = eventSession === sessionStore.activeSessionKey.value;
	const isChatPage = currentPrefix === "/chats";

	if (isActive && sessionStore.switchInProgress.value) {
		// If session switching got stuck (e.g. lost RPC response), do not drop
		// terminal frames. Unstick and process final/error so replies still show
		// without requiring a full page reload.
		const allowDuringSwitch =
			p.state === "user_message" ||
			p.state === "final" ||
			p.state === "error" ||
			p.state === "aborted" ||
			p.state === "notice" ||
			p.state === "session_cleared" ||
			p.state === "queue_cleared";
		if (!allowDuringSwitch) {
			return;
		}
		if (p.state === "final" || p.state === "error" || p.state === "aborted") {
			sessionStore.switchInProgress.value = false;
			S.setSessionSwitchInProgress(false);
		}
	}

	if (p.sessionKey && !sessionStore.getByKey(p.sessionKey)) {
		fetchSessions();
	}

	const handler = chatHandlers[p.state || ""];
	if (handler) handler(p, isActive, isChatPage, eventSession);
}
