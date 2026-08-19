// ── Chat event handler functions ──────────────────────────────

import {
	appendReasoningDisclosure,
	chatAddErrorCard,
	chatAddErrorMsg,
	chatAddMsg,
	setComposerStopButton,
	smartScrollToBottom,
	updateTokenBar,
} from "../chat-ui";
import { highlightCodeBlocks } from "../code-highlight";
import { unmountExecuteCommandToolBubbles } from "../components/ExecuteCommandToolBubble";
import { localizeStructuredError, renderAudioPlayer, renderMarkdown } from "../helpers";
import { t } from "../i18n";
import { appendMessageActions } from "../message-actions";
import { maybeRefreshFullContext } from "../pages/ChatPage";
import { renderCheckpointCard } from "../pages/chat/context-card";
import { setQueuedPrompts } from "../pages/chat/prompt-queue";
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
import {
	applyProviderItemUpdate,
	extractSegmentReasoning,
	type ProviderSegmentViewModel,
} from "../sessions/provider-segment-reducer";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import { terminalContextTokens } from "../terminal-usage";
import { resolveAssistantTurnEnd, toolCallIds } from "../tool-call-card";
import { isToolLifecyclePayload, type ToolInvocationSnapshot, toToolLifecycleEvent } from "../tool-lifecycle";
import type { HistoryMessage } from "../types/session";
import type {
	AbortedPartialState,
	AssistantHistoryMessage,
	ChatError,
	ChatPayload,
	ProviderItemUpdate,
	ProviderUpdatePayload,
	ReasoningContent,
	ToolLifecyclePayload,
} from "../types/ws-events";
import { hasVisibleReasoning, isReasoningContent } from "../types/ws-events";
import { currentLiveSegment, liveSegmentFor, openLiveSegment } from "./live-segments";
import { placeSegmentNode } from "./segment-placement";
import {
	clearChatEmptyState,
	hasNonWhitespaceContent,
	setSafeMarkdownHtml,
	updateSessionHistoryIndex,
	updateSessionRunId,
} from "./shared";
import {
	clearStaleRunningToolCards,
	clearToolLifecycleStateForSession,
	reduceLiveToolInvocation,
	renderAbortedPartialInDom,
	renderChannelUserMessage,
	renderToolLifecycleSnapshot,
	resolveFinalMessageEl,
} from "./tool-helpers";

export type ChatHandler = (p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string) => void;

// ── Individual chat event handlers ────────────────────────────

function sessionMediaUrl(sessionKey: string, audioPath: string): string | null {
	const filename = audioPath.split("/").pop() || "";
	if (!filename) return null;
	return `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(filename)}`;
}

function assistantHistoryMessage(message: AssistantHistoryMessage): HistoryMessage {
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
		// Canonical identity travels with the message: a client that reopens the
		// chat needs it to recognize this turn as the segment whose records it
		// already applied, instead of rendering the segment a second time.
		providerItems: message.providerItems,
		segmentId: message.segmentId,
		audio: message.audio,
		run_id: message.run_id,
		created_at: message.created_at,
		seq: message.seq,
	};
}

function createLiveAssistantSegment(): HTMLElement {
	clearChatEmptyState();
	const segment = document.createElement("div");
	segment.className = "msg assistant reasoning-stream";
	S.chatMsgBox?.appendChild(segment);
	S.setStreamEl(segment);
	S.setStreamText("");
	return segment;
}

function activeAssistantSegment(): HTMLElement {
	return S.streamEl || createLiveAssistantSegment();
}

function liveReasoningContent(segment: HTMLElement): ReasoningContent {
	const encoded = segment.querySelector<HTMLElement>(".msg-reasoning-body")?.dataset.reasoning;
	if (!encoded) return "";
	try {
		const value: unknown = JSON.parse(encoded);
		return isReasoningContent(value) ? value : "";
	} catch {
		return "";
	}
}

function finishLiveReasoning(segment: HTMLElement, reasoningContent?: ReasoningContent): void {
	segment.querySelector(".thinking-status")?.remove();
	segment.classList.remove("reasoning-stream");
	const reasoning = reasoningContent ?? liveReasoningContent(segment);
	if (hasVisibleReasoning(reasoning) || segment.querySelector(".msg-reasoning")) {
		appendReasoningDisclosure(segment, reasoning, { expanded: false, streaming: false });
	}
}

function handleChatThinking(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	const segment = activeAssistantSegment();
	segment.classList.add("reasoning-stream");
	appendReasoningDisclosure(segment, liveReasoningContent(segment), { expanded: true, streaming: true });
	smartScrollToBottom();
}

function handleChatSegmentStart(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (p.segmentId) {
		const previous = currentLiveSegment(eventSession);
		openLiveSegment(eventSession, p.segmentId);
		// A new segment gets its own bubble. Keeping the previous element would
		// append this segment's text to the one a retry just closed, showing the
		// two attempts as a single run-on answer.
		if (previous && previous.segmentId !== p.segmentId) detachLiveAssistantSegment();
	}
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
}

/// Close the live assistant element so the next segment starts a new one.
///
/// A segment that produced nothing visible leaves no element behind: an empty
/// bubble has nothing to show and its disclosure would keep announcing a
/// thinking state no later event can end. Anything else is finished in place,
/// because it holds what the closed segment produced and the next segment must
/// not erase it.
function detachLiveAssistantSegment(): void {
	const segment = S.streamEl;
	if (!segment) return;
	S.setStreamEl(null);
	S.setStreamText("");
	if (renderedSegmentIsEmpty(segment)) {
		segment.remove();
		return;
	}
	finishLiveReasoning(segment);
}

/// Whether the element shows nothing: no rendered text and no reasoning parts.
function renderedSegmentIsEmpty(segment: HTMLElement): boolean {
	const text = segment.querySelector<HTMLElement>(":scope > .msg-markdown-body")?.textContent;
	if (hasNonWhitespaceContent(text)) return false;
	return !hasVisibleReasoning(liveReasoningContent(segment));
}

function handleChatProviderUpdate(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!p.update) return;
	const payload = p as ProviderUpdatePayload;
	cacheIndexedHistoryMessage(
		eventSession,
		{
			role: "provider_update",
			update: payload.update,
			created_at: Date.now(),
			run_id: p.runId,
		},
		payload.historyIndex,
	);
	// The live segment reducer owns identity and ordering for this session.
	const segment = liveSegmentFor(eventSession, payload.update.segmentId);
	applyProviderItemUpdate(segment, payload.update);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	placeLiveAssistantSegment(activeAssistantSegment(), segment);
	// Function call deltas belong to a tool card, not to the assistant bubble.
	// Re-rendering the reasoning for them would repaint a bubble whose content
	// did not change.
	if (touchesReasoning(payload.update.payload)) renderLiveSegmentReasoning(segment);
}

/// Whether an update changes what the reasoning disclosure displays.
function touchesReasoning(payload: ProviderItemUpdate["payload"]): boolean {
	switch (payload.update_type) {
		case "reasoning_delta":
		case "reasoning_part_done":
		case "reasoning_item_done":
		case "reasoning_text":
		case "reasoning_text_delta":
			return true;
		default:
			return false;
	}
}

/// Place the live assistant node at the canonical position of the first
/// non-function-call item of the segment, so it keeps its slot relative to the
/// tool cards of the same segment.
function placeLiveAssistantSegment(element: HTMLElement, segment: ProviderSegmentViewModel): void {
	const anchor = segment.items.find((item) => item.payload.type !== "function_call");
	if (!(anchor && S.chatMsgBox)) return;
	placeSegmentNode(S.chatMsgBox, element, segment.segmentId, anchor.position);
}

function renderLiveSegmentReasoning(segment: ProviderSegmentViewModel): void {
	const reasoning = extractSegmentReasoning(segment);
	if (!hasVisibleReasoning(reasoning)) return;
	const element = activeAssistantSegment();
	element.classList.add("reasoning-stream");
	appendReasoningDisclosure(element, reasoning, { expanded: true, streaming: true });
	smartScrollToBottom();
}

function handleChatProviderSegmentClose(
	p: ChatPayload,
	isActive: boolean,
	isChatPage: boolean,
	eventSession: string,
): void {
	if (p.historyIndex !== undefined) {
		cacheIndexedHistoryMessage(
			eventSession,
			{
				role: "provider_segment_close",
				segmentId: p.segmentId,
				outcome: p.outcome,
				created_at: Date.now(),
				run_id: p.runId,
			},
			p.historyIndex,
		);
	}
	const segment = currentLiveSegment(eventSession);
	if (segment && p.outcome) segment.outcome = p.outcome;
	if (!(isActive && isChatPage)) return;
	if (S.streamEl) {
		finishLiveReasoning(S.streamEl, segment ? extractSegmentReasoning(segment) : undefined);
	}
}

function handleChatThinkingDone(_p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	if (!(isActive && isChatPage)) return;
	if (S.streamEl) finishLiveReasoning(S.streamEl);
}

function handleChatVoicePending(_p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	// Update per-session signal
	const session = sessionStore.getByKey(eventSession);
	if (session) session.voicePending.value = true;
	if (!(isActive && isChatPage)) return;
	// Dual-write to global state for backward compat
	S.setVoicePending(true);
	// Keep the active reasoning part visible while audio is prepared.
}

function cacheIndexedHistoryMessage(
	eventSession: string,
	message: HistoryMessage,
	historyIndex: number | undefined,
): void {
	if (!Number.isInteger(historyIndex)) return;
	const index = historyIndex as number;
	const session = sessionStore.getByKey(eventSession);
	const knownIndex = session ? session.lastHistoryIndex.value : S.lastHistoryIndex;
	if (index > knownIndex) bumpSessionCount(eventSession, 1);
	cacheSessionHistoryMessage(eventSession, message, index);
	updateSessionHistoryIndex(eventSession, index);
}

function cacheLifecycleAssistantFrame(payload: ToolLifecyclePayload, eventSession: string): void {
	if (payload.stage !== "input_ready" || !payload.assistantMessage) return;
	cacheIndexedHistoryMessage(
		eventSession,
		assistantHistoryMessage(payload.assistantMessage),
		payload.assistantMessageIndex,
	);
}

function cacheToolLifecycleFrame(snapshot: ToolInvocationSnapshot, eventSession: string): void {
	const inserted = cacheSessionHistoryMessage(eventSession, {
		role: "tool_lifecycle",
		...toToolLifecycleEvent(snapshot.lifecycle),
		accumulatedArguments: snapshot.accumulatedArguments,
		created_at: snapshot.lifecycle.emittedAtMs,
	});
	if (inserted) bumpSessionCount(eventSession, 1);
}

function updateActiveTokenBarContextBudget(p: ChatPayload, isActive: boolean, isChatPage: boolean): void {
	if (isActive && isChatPage && p.contextBudget) updateTokenBar(p.contextBudget);
}

function handleChatToolLifecycle(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	if (!isToolLifecyclePayload(p)) return;
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	cacheLifecycleAssistantFrame(p, eventSession);
	const snapshot = reduceLiveToolInvocation(eventSession, p);
	cacheToolLifecycleFrame(snapshot, eventSession);
	updateActiveTokenBarContextBudget(p, isActive, isChatPage);
	const toolSession = sessionStore.getByKey(eventSession);
	if (p.stage === "input_ready" && toolSession) toolSession.streamText.value = "";
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);
	renderToolLifecycleSnapshot(snapshot, eventSession);
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
			content: typeof p.text === "string" ? p.text : "",
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
	// Suppress the echo for the originating client. Prompts replayed from the
	// queue are exempt: the submitting client removed its optimistic bubble
	// when the prompt was queued, so suppressing them here would hide the
	// message on that client while every other client renders it.
	if (!p.replayed && p.seq !== undefined && p.seq !== null && p.seq <= S.chatSeq) return;
	const msgSession = sessionStore.getByKey(eventSession);
	const lastIdx = msgSession ? msgSession.lastHistoryIndex.value : -1;
	if (p.messageIndex !== undefined && p.messageIndex !== null && p.messageIndex <= lastIdx) return;

	bumpSessionCount(eventSession, 1);
	cacheSessionHistoryMessage(
		eventSession,
		{
			role: "user",
			content: typeof p.text === "string" ? p.text : "",
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
	chatAddMsg("user", renderMarkdown(typeof p.text === "string" ? p.text : ""), true);
}

function handleChatDelta(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	if (typeof p.text !== "string" || !p.text) return;
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
	// The active reasoning part remains available for a following tool call.
	if (!(S.streamEl || p.text.trim())) return;
	const streamElement = activeAssistantSegment();
	streamElement.classList.remove("reasoning-stream");
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
	const text = typeof payload.text === "string" ? payload.text : "";
	return {
		text,
		hasVisibleContent:
			hasNonWhitespaceContent(text) ||
			hasVisibleReasoning(payload.reasoning) ||
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
		// The cached message stands in for the persisted one until the next
		// reload, so it carries the same canonical identity: without it the
		// segment records replay as a separate message beside this turn.
		providerItems: payload.providerItems,
		segmentId: payload.segmentId,
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
	clearStaleRunningToolCards();
}

function appendFinalText(messageElement: HTMLElement, text: string): void {
	if (!hasNonWhitespaceContent(text)) return;
	const textWrap = document.createElement("div");
	textWrap.className = "mt-2";
	setSafeMarkdownHtml(textWrap, text);
	messageElement.appendChild(textWrap);
}

function applyFinalReasoning(messageElement: HTMLElement, reasoning: ReasoningContent | undefined): void {
	messageElement.classList.remove("reasoning-stream");
	messageElement.querySelector(".thinking-status")?.remove();
	const disclosure = messageElement.querySelector(".msg-reasoning");
	if (hasVisibleReasoning(reasoning)) {
		appendReasoningDisclosure(messageElement, reasoning, { expanded: false, streaming: false });
	} else {
		disclosure?.remove();
	}
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
	applyFinalReasoning(messageElement, payload.reasoning);
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
	const reasoning = messageElement.querySelector(":scope > .msg-reasoning");
	if (reasoning) reasoning.remove();
	messageElement.textContent = "";
	if (reasoning) messageElement.appendChild(reasoning);
	if (audioSource) renderAudioPlayer(messageElement, audioSource, true);
	appendFinalText(messageElement, text);
}

function renderStreamedFinal(payload: ChatPayload, text: string): HTMLElement | null {
	let messageElement = resolveFinalMessageEl(payload);
	if (payload.replyMedium === "voice" && payload.audio) {
		messageElement ||= chatAddMsg("assistant", "", false);
		if (messageElement) renderStreamedVoiceAudio(payload, text, messageElement);
	}
	if (messageElement) applyFinalReasoning(messageElement, payload.reasoning);
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
}

function handleChatFinal(payload: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearToolLifecycleStateForSession(eventSession);
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

function structuredChatError(p: ChatPayload): ChatError | null {
	return p.error && typeof p.error === "object" ? p.error : null;
}

function handleAutoCompactError(p: ChatPayload, activePage: boolean): void {
	if (!activePage) return;
	removeCompactingStatus(p);
	const error = structuredChatError(p);
	chatAddMsg("error", `Auto-compact failed: ${error?.message || error?.detail || "unknown error"}`);
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
	const error = structuredChatError(p);
	if (error?.retryAfterMs !== undefined && error.retryAfterMs !== null) return Number(error.retryAfterMs) || 0;
	return 0;
}

function retryStatusText(p: ChatPayload): string {
	const retryMs = retryDelayMsFromPayload(p);
	const retrySecs = Math.max(1, Math.ceil(retryMs / 1000));
	const rateLimited = structuredChatError(p)?.type === "rate_limit_exceeded";
	return rateLimited
		? `Rate limited by provider, retrying in ${retrySecs}s\u2026`
		: `Temporary provider issue, retrying in ${retrySecs}s\u2026`;
}

function handleChatRetrying(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	updateSessionRunId(eventSession, p.runId);
	setSessionReplying(eventSession, true);
	if (!(isActive && isChatPage)) return;
	setComposerStopButton(true, eventSession);

	const segment = activeAssistantSegment();
	let status = segment.querySelector<HTMLElement>(".thinking-status");
	if (!status) {
		status = document.createElement("div");
		status.className = "thinking-status";
		segment.appendChild(status);
	}
	status.textContent = retryStatusText(p);
	smartScrollToBottom();
}

// ── Error / abort / notice / clear ────────────────────────────

function renderChatErrorMessage(p: ChatPayload): void {
	const error = structuredChatError(p);
	if (error?.title) {
		chatAddErrorCard(localizeStructuredError(error) as Parameters<typeof chatAddErrorCard>[0]);
		return;
	}
	chatAddErrorMsg(p.message || "unknown");
}

function appendErrorContinueButton(p: ChatPayload): void {
	if (!structuredChatError(p)?.canContinue) return;
	const lastCard = S.chatMsgBox?.querySelector(".error-card:last-child") as HTMLElement | null;
	const body = lastCard?.querySelector(".error-body");
	if (!body) return;
	const button = document.createElement("button");
	button.className = "provider-btn error-continue-btn";
	button.textContent = t("errors:chat.continue", "Continue");
	button.onclick = () => {
		button.disabled = true;
		button.textContent = t("errors:chat.continuing", "Continuing...");
		(S.chatInput as HTMLInputElement).value = t("errors:chat.continueMessage", "Please continue where you left off.");
		// Trigger send by clicking the chat send button (sendChat is local to ChatPage)
		S.chatSendBtn?.click();
	};
	body.appendChild(button);
}

function renderActiveChatError(p: ChatPayload, partialState: AbortedPartialState): void {
	setComposerStopButton(false);
	clearStaleRunningToolCards();
	if (!partialState.hasVisiblePartial) S.streamEl?.remove();
	renderAbortedPartialInDom(p, partialState);
	renderChatErrorMessage(p);
	appendErrorContinueButton(p);
	S.setStreamEl(null);
	S.setStreamText("");
	S.setVoicePending(false);
}

function handleChatError(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearToolLifecycleStateForSession(eventSession);
	setSessionReplying(eventSession, false);
	setSessionActiveRunId(eventSession, null);
	const partialState = getAbortedPartialState(p);
	const errSession = sessionStore.getByKey(eventSession);
	cacheAbortedPartial(eventSession, p, errSession, partialState);
	errSession?.resetStreamState();
	if (hasAbortedPartial(partialState) && !isActive) setSessionUnread(eventSession, true);
	if (isActive && isChatPage) {
		renderActiveChatError(p, partialState);
		return;
	}
	S.setVoicePending(false);
}

function getAbortedPartialState(p: ChatPayload): AbortedPartialState {
	const partial = p.partialMessage && typeof p.partialMessage === "object" ? p.partialMessage : null;
	const partialText = String(partial?.content || "");
	const partialReasoning = partial?.reasoning || "";
	return {
		partial,
		partialText,
		partialReasoning,
		hasVisiblePartial: hasNonWhitespaceContent(partialText) || hasVisibleReasoning(partialReasoning),
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
	clearStaleRunningToolCards();
	if (!partialState.hasVisiblePartial) S.streamEl?.remove();
	renderAbortedPartialInDom(p, partialState);
	S.setStreamEl(null);
	S.setStreamText("");
	S.setVoicePending(false);
}

function handleChatAborted(p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearToolLifecycleStateForSession(eventSession);
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

function handleChatPromptQueue(p: ChatPayload, _isActive: boolean, _isChatPage: boolean, eventSession: string): void {
	setQueuedPrompts(eventSession, p.prompts ?? []);
}

function handleChatSessionCleared(_p: ChatPayload, isActive: boolean, isChatPage: boolean, eventSession: string): void {
	clearToolLifecycleStateForSession(eventSession);
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
	if (S.chatMsgBox) {
		unmountExecuteCommandToolBubbles(S.chatMsgBox);
		S.chatMsgBox.textContent = "";
	}
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	updateTokenBar();
}

// ── Handler map and dispatcher ────────────────────────────────

export const chatHandlers: Record<string, ChatHandler> = {
	thinking: handleChatThinking,
	thinking_done: handleChatThinkingDone,
	segment_start: handleChatSegmentStart,
	provider_update: handleChatProviderUpdate,
	provider_segment_close: handleChatProviderSegmentClose,
	voice_pending: handleChatVoicePending,
	tool_lifecycle: handleChatToolLifecycle,
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
	prompt_queue: handleChatPromptQueue,
	session_cleared: handleChatSessionCleared,
};

export function handleChatEvent(p: ChatPayload): void {
	const eventSession = p.sessionKey || sessionStore.activeSessionKey.value;
	const isActive = eventSession === sessionStore.activeSessionKey.value;
	const isChatPage = currentPrefix === "/chats";

	if (isActive && sessionStore.switchInProgress.value) {
		// If session switching got stuck (e.g. lost RPC response), do not drop
		// persisted lifecycle or terminal frames.
		// `segment_start`, `provider_update` and `provider_segment_close` are
		// persisted records: dropping one leaves the segment reducer with a hole
		// it cannot rebuild, and the identity of every later update is rejected.
		const allowDuringSwitch =
			p.state === "user_message" ||
			p.state === "tool_lifecycle" ||
			p.state === "segment_start" ||
			p.state === "provider_update" ||
			p.state === "provider_segment_close" ||
			p.state === "final" ||
			p.state === "error" ||
			p.state === "aborted" ||
			p.state === "notice" ||
			p.state === "session_cleared" ||
			p.state === "prompt_queue";
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
