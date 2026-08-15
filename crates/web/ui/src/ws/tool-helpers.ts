// ── Tool call utilities ───────────────────────────────────────

import { completeA2uiToolCard, isA2uiTool, mountA2uiToolCard } from "../a2ui-renderer";
import type { ChannelFooterInfo } from "../chat-ui";
import {
	appendChannelFooter,
	appendReasoningDisclosure,
	chatAddMsg,
	smartScrollToBottom,
	stripChannelPrefix,
} from "../chat-ui";
import { mountExecuteCommandToolBubble, unmountExecuteCommandToolBubble } from "../components/ExecuteCommandToolBubble";
import { renderAudioPlayer, renderMarkdown } from "../helpers";
import { appendMessageActions } from "../message-actions";
import { navigate } from "../router";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import {
	appendToolCardContextBudget,
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	isCommandToolName,
	normalizeToolResult,
	renderToolCardError,
	renderToolCardResult,
	resolveToolBatchEnd,
	setToolCardExpanded,
	setToolCardStatus,
	toolCallIds,
} from "../tool-call-card";
import {
	type AbortedPartialState,
	type ChatPayload,
	hasVisibleReasoning,
	type ToolCallPayload,
	type ToolResult,
} from "../types/ws-events";
import { clearChatEmptyState, hasNonWhitespaceContent, setSafeMarkdownHtml } from "./shared";

// ── Pending tool call end tracking ────────────────────────────

export const pendingToolCallEnds: Map<string, ToolCallPayload> = new Map();

export function toolCallLogicalId(payload: ToolCallPayload | null | undefined): string {
	if (!payload) return "";
	const toolCallId = payload.toolCallId || "";
	if (payload.runId) return `${payload.runId}:${toolCallId}`;
	return String(toolCallId);
}

export function toolCallCardId(payload: ToolCallPayload | ChatPayload | null | undefined): string {
	const p = payload as ToolCallPayload | null | undefined;
	const toolCallId = p?.toolCallId || "";
	if (p?.runId) {
		return `tool-${p.runId}-${toolCallId}`;
	}
	return `tool-${toolCallId}`;
}

export function toolCallEventKey(
	eventSession: string,
	payload: ToolCallPayload | ChatPayload | null | undefined,
): string {
	return `${eventSession}:${toolCallLogicalId(payload as ToolCallPayload)}`;
}

export function clearPendingToolCallEndsForSession(sessionKey: string): void {
	const prefix = `${sessionKey}:`;
	for (const key of pendingToolCallEnds.keys()) {
		if (key.startsWith(prefix)) {
			pendingToolCallEnds.delete(key);
		}
	}
}

export function createToolCallCardForPayload(p: ChatPayload): HTMLElement | null {
	const cardId = toolCallCardId(p);
	const existing = document.getElementById(cardId) as HTMLElement | null;
	if (existing) return existing;
	if (!S.chatMsgBox) return null;
	const card = createToolCallCard({
		id: cardId,
		toolCallId: p.toolCallId,
		toolName: p.toolName,
		arguments: p.arguments,
		executionMode: p.executionMode,
		status: "running",
		expanded: true,
	});
	if (isA2uiTool(p.toolName) && p.rejected !== true) {
		mountA2uiToolCard(card, {
			arguments: p.arguments,
			runId: p.runId,
			toolCallId: p.toolCallId,
			interactive: true,
		});
	}
	clearChatEmptyState();
	S.chatMsgBox.appendChild(card);
	smartScrollToBottom();
	return card;
}

// ── Tool result rendering ─────────────────────────────────────

export function appendToolResult(toolCard: HTMLElement, resultValue: ToolResult | string, eventSession: string): void {
	const result = normalizeToolResult(resultValue);
	const out = (result.stdout || result.output || "").replace(/\n+$/, "");
	// Update per-session signal
	const toolSession = sessionStore.getByKey(eventSession);
	if (toolSession) toolSession.lastToolOutput.value = out;
	// Dual-write to global state for backward compat
	S.setLastToolOutput(out);
	renderToolCardResult(toolCard, result, { sessionKey: eventSession || S.activeSessionKey || "main" });
}

// ── Tool card completion ──────────────────────────────────────

function isToolValidationErrorPayload(p: ChatPayload): boolean {
	if (p.rejected === true) return true;
	const errorDetail = p.error?.detail || p.error?.message;
	if (!(p && !p.success && errorDetail)) return false;
	const errDetail = errorDetail.toLowerCase();
	return (
		errDetail.includes("missing field") ||
		errDetail.includes("missing required") ||
		errDetail.includes("missing 'action'") ||
		errDetail.includes("missing 'url'")
	);
}

function setCompletedToolStatus(toolCard: HTMLElement, success: boolean | undefined, validationError: boolean): void {
	setToolCardStatus(toolCard, validationError ? "retry" : success ? "success" : "error");
}

function renderCompletedToolResult(
	toolCard: HTMLElement,
	payload: ChatPayload,
	eventSession: string,
	validationError: boolean,
): void {
	if (payload.result) {
		appendToolResult(toolCard, payload.result, eventSession);
		if (!payload.success && payload.error) appendToolCardError(toolCard, payload.error, validationError);
		return;
	}
	if (payload.success) {
		renderToolCardResult(toolCard, {}, { sessionKey: eventSession || S.activeSessionKey || "main" });
		return;
	}
	if (payload.error) renderToolCardError(toolCard, payload.error, validationError);
}

function completeToolA2ui(toolCard: HTMLElement, payload: ChatPayload): void {
	if (!isA2uiTool(payload.toolName) || payload.rejected === true) return;
	completeA2uiToolCard(
		toolCard,
		payload.success === true,
		payload.result,
		payload.error?.detail || payload.error?.message,
	);
}

function appendSkillChangeHint(toolCard: HTMLElement, payload: ChatPayload): void {
	if (!payload.success || (payload.toolName !== "create_skill" && payload.toolName !== "update_skill")) return;
	const hint = document.createElement("div");
	hint.className = "skill-hint";
	const verb = payload.toolName === "create_skill" ? "created" : "updated";
	const link = document.createElement("a");
	link.href = "/skills";
	link.textContent = "personal skills";
	link.addEventListener("click", (event: MouseEvent) => {
		event.preventDefault();
		navigate("/skills");
	});
	hint.append(`Skill ${verb} \u2014 available in your `, link);
	getToolCardDetailsContainer(toolCard).appendChild(hint);
}

export function completeToolCard(toolCard: HTMLElement, p: ChatPayload, eventSession: string): void {
	unmountExecuteCommandToolBubble(toolCard);
	const validationError = isToolValidationErrorPayload(p);
	setCompletedToolStatus(toolCard, p.success, validationError);
	renderCompletedToolResult(toolCard, p, eventSession, validationError);
	appendToolCardContextBudget(toolCard, p.contextBudget);
	// The A2UI surface lives beside the standard Parameters/Result/Context
	// budget disclosures, so it is refreshed after they are rendered.
	completeToolA2ui(toolCard, p);
	setToolCardExpanded(toolCard, validationError || isCommandToolName(p.toolName) || isA2uiTool(p.toolName));
	appendSkillChangeHint(toolCard, p);
}

export function clearStaleRunningToolCards(): void {
	if (!S.chatMsgBox) return;
	const statusEls = S.chatMsgBox.querySelectorAll(".msg.command-card .command-status");
	for (const statusEl of statusEls) {
		const card = statusEl.closest(".msg.command-card") as HTMLElement | null;
		if (!card) continue;
		if (!card.classList.contains("running")) continue;
		if (card.classList.contains("tool-call-card")) {
			unmountExecuteCommandToolBubble(card);
			setToolCardStatus(card, "success");
			setToolCardExpanded(card, false);
			continue;
		}
		statusEl.remove();
		if (!(card.classList.contains("command-ok") || card.classList.contains("command-err"))) {
			card.className = "msg command-card command-ok";
		}
	}
}

// ── Tool call start ───────────────────────────────────────────

/** Close the live assistant segment that precedes a tool card.
 *
 * The server persisted the assistant frame before it emitted the tool event.
 * Binding the live element to that canonical history index and clearing the
 * stream state is what keeps the next iteration's deltas below the tool card
 * instead of appending them to a segment that now sits above it.
 *
 * `assistantHistoryIndex` is the index of the assistant frame itself: tool
 * start carries it in `messageIndex`, while a rejected call reports it
 * separately because its `messageIndex` addresses the tool-result record.
 */
function assistantSegmentByHistoryIndex(historyIndex: number | undefined): HTMLElement | null {
	if (!Number.isInteger(historyIndex)) return null;
	return S.chatMsgBox?.querySelector(`.msg.assistant[data-history-index="${historyIndex}"]`) as HTMLElement | null;
}

function applyCanonicalAssistantSegment(
	segment: HTMLElement,
	assistantMessage: ChatPayload["assistantMessage"],
	historyIndex: number | undefined,
	sessionKey: string,
): void {
	const text = assistantMessage?.content || "";
	const reasoning = assistantMessage?.reasoning || "";
	setSafeMarkdownHtml(segment, text);
	segment.classList.remove("reasoning-stream");
	segment.querySelector(".thinking-status")?.remove();
	const disclosure = segment.querySelector(".msg-reasoning");
	if (hasVisibleReasoning(reasoning)) {
		appendReasoningDisclosure(segment, reasoning, { expanded: false, streaming: false });
	} else {
		disclosure?.remove();
	}
	if (Number.isInteger(historyIndex)) segment.dataset.historyIndex = String(historyIndex);
	appendMessageActions({
		messageEl: segment,
		sessionKey,
		messageIndex: historyIndex,
		text,
		hasAudio: Boolean(assistantMessage?.audio),
	});
}

function closeCurrentStreamSegment(
	assistantMessage: ChatPayload["assistantMessage"],
	historyIndex: number | undefined,
	sessionKey: string,
	hasCanonicalContent: boolean,
): boolean {
	const streamElement = S.streamEl;
	if (!streamElement) return false;
	if (hasCanonicalContent) {
		applyCanonicalAssistantSegment(streamElement, assistantMessage, historyIndex, sessionKey);
	} else {
		streamElement.remove();
	}
	S.setStreamEl(null);
	S.setStreamText("");
	return true;
}

export function closeLiveAssistantSegment(
	assistantMessage: ChatPayload["assistantMessage"],
	assistantHistoryIndex: number | undefined,
	sessionKey: string,
): void {
	const canonicalText = assistantMessage?.content || "";
	const canonicalReasoning = assistantMessage?.reasoning || "";
	const hasCanonicalContent = hasNonWhitespaceContent(canonicalText) || hasVisibleReasoning(canonicalReasoning);
	if (closeCurrentStreamSegment(assistantMessage, assistantHistoryIndex, sessionKey, hasCanonicalContent)) return;
	if (!hasCanonicalContent) return;
	const segment =
		assistantSegmentByHistoryIndex(assistantHistoryIndex) ||
		chatAddMsg("assistant", renderMarkdown(canonicalText), true);
	if (segment) applyCanonicalAssistantSegment(segment, assistantMessage, assistantHistoryIndex, sessionKey);
}

export function handleToolCallStartDom(p: ChatPayload, eventSession: string): void {
	closeLiveAssistantSegment(p.assistantMessage, p.messageIndex, eventSession);
	const cardId = toolCallCardId(p);
	const existingCard = document.getElementById(cardId) as HTMLElement | null;
	if (existingCard) {
		if (Number.isInteger(p.messageIndex)) {
			existingCard.dataset.assistantHistoryIndex = String(p.messageIndex);
		}
		if (isCommandToolName(p.toolName) && p.toolCallId) {
			mountExecuteCommandToolBubble(existingCard, {
				toolCallId: p.toolCallId,
				sessionKey: eventSession,
				startedAt: p.startedAt ?? Number.NaN,
			});
		}
		if (isA2uiTool(p.toolName)) {
			mountA2uiToolCard(existingCard, {
				arguments: p.arguments,
				runId: p.runId,
				toolCallId: p.toolCallId,
				interactive: true,
			});
		}
		return;
	}
	const card = createToolCallCard({
		id: cardId,
		toolCallId: p.toolCallId,
		assistantHistoryIndex: Number.isInteger(p.messageIndex) ? p.messageIndex : undefined,
		toolName: p.toolName,
		arguments: p.arguments,
		executionMode: p.executionMode,
		status: "running",
		expanded: true,
	});
	if (isCommandToolName(p.toolName) && p.toolCallId) {
		mountExecuteCommandToolBubble(card, {
			toolCallId: p.toolCallId,
			sessionKey: eventSession,
			startedAt: p.startedAt ?? Number.NaN,
		});
	}
	if (isA2uiTool(p.toolName)) {
		mountA2uiToolCard(card, {
			arguments: p.arguments,
			runId: p.runId,
			toolCallId: p.toolCallId,
			interactive: true,
		});
	}
	clearChatEmptyState();
	S.chatMsgBox?.appendChild(card);
	const endKey = toolCallEventKey(eventSession, p);
	const pendingEnd = pendingToolCallEnds.get(endKey);
	if (pendingEnd) {
		pendingToolCallEnds.delete(endKey);
		completeToolCard(card, pendingEnd as ChatPayload, eventSession);
	}
	smartScrollToBottom();
}

// ── Channel user message rendering ────────────────────────────

export function renderChannelUserMessage(p: ChatPayload, _eventSession: string): void {
	// Compare against the per-session history index, not the global one,
	// to avoid skipping events when viewing a different session.
	const chanSession = sessionStore.getByKey(p.sessionKey || S.activeSessionKey);
	const chanLastIdx = chanSession ? chanSession.lastHistoryIndex.value : S.lastHistoryIndex;
	if (p.messageIndex !== undefined && p.messageIndex <= chanLastIdx) return;

	const cleanText = stripChannelPrefix(typeof p.text === "string" ? p.text : "");
	const sessionKey = p.sessionKey || S.activeSessionKey;
	const audioFilename = p.channel?.audio_filename;
	let el: HTMLElement | null;
	if (audioFilename) {
		el = chatAddMsg("user", "", true);
		if (el) {
			const audioSrc = `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(audioFilename)}`;
			renderAudioPlayer(el, audioSrc);
			if (cleanText) {
				const textWrap = document.createElement("div");
				textWrap.className = "mt-2";
				// Safe: renderMarkdown calls esc() first -- all user input is
				// HTML-escaped before formatting tags are applied.
				setSafeMarkdownHtml(textWrap, cleanText);
				el.appendChild(textWrap);
			}
		}
	} else {
		el = chatAddMsg("user", renderMarkdown(cleanText), true);
	}
	if (el && p.channel) {
		appendChannelFooter(el, p.channel as ChannelFooterInfo);
	}
}

// ── Final message resolution ──────────────────────────────────

function normalizeEchoComparable(text: string | null | undefined): string {
	if (!text) return "";
	return text
		.replace(/```[a-zA-Z0-9_-]*\n?/g, "")
		.replace(/```/g, "")
		.replace(/[`\s]/g, "");
}

function isPureToolOutputEcho(finalText: string, toolOutput: string): boolean {
	const finalComparable = normalizeEchoComparable(finalText);
	const toolComparable = normalizeEchoComparable(toolOutput);
	if (!(finalComparable && toolComparable)) return false;
	return finalComparable === toolComparable;
}

function removeStreamElement(): void {
	S.streamEl?.remove();
}

function persistedAssistantMessage(historyIndex: number | undefined): HTMLElement | null {
	if (!Number.isInteger(historyIndex)) return null;
	return S.chatMsgBox?.querySelector(`.msg.assistant[data-history-index="${historyIndex}"]`) as HTMLElement | null;
}

function renderFinalSegment(finalText: string, historyIndex: number | undefined): HTMLElement | null {
	if (S.streamEl) {
		setSafeMarkdownHtml(S.streamEl, finalText);
		return S.streamEl;
	}
	return persistedAssistantMessage(historyIndex) || chatAddMsg("assistant", renderMarkdown(finalText), true);
}

export function resolveFinalMessageEl(p: ChatPayload): HTMLElement | null {
	const finalText = typeof p.text === "string" ? p.text : "";
	const finalReasoning = p.reasoning || "";
	if (!(hasNonWhitespaceContent(finalText) || hasVisibleReasoning(finalReasoning))) {
		removeStreamElement();
		return null;
	}
	if (isPureToolOutputEcho(finalText, S.lastToolOutput) && !hasVisibleReasoning(finalReasoning)) {
		removeStreamElement();
		return null;
	}
	const visibleText = isPureToolOutputEcho(finalText, S.lastToolOutput) ? "" : finalText;
	return renderFinalSegment(visibleText, p.messageIndex);
}

// ── Terminal metadata ─────────────────────────────────────────

export function appendTerminalMetadataForPartial(
	p: ChatPayload,
	partial: ChatPayload["partialMessage"] | null,
	anchor: HTMLElement | null,
): HTMLElement | null {
	return appendTerminalMetadata(
		S.chatMsgBox,
		anchor,
		terminalMetadataData(partial || {}, {
			replyMedium: p.replyMedium || "text",
			historyIndex: p.messageIndex,
			runId: p.runId,
			timestamp: Date.now(),
		}),
	);
}

// ── Aborted partial rendering ─────────────────────────────────
function abortedToolBatchEnd(partialState: AbortedPartialState): HTMLElement | null {
	return partialState.hasTerminalToolBatch ? resolveToolBatchEnd(toolCallIds(partialState.partial?.tool_calls)) : null;
}

function renderMetadataOnlyPartial(p: ChatPayload, partialState: AbortedPartialState): void {
	const toolBatchEnd = abortedToolBatchEnd(partialState);
	if (toolBatchEnd && appendTerminalMetadataForPartial(p, partialState.partial, toolBatchEnd)) smartScrollToBottom();
}

function assignPartialHistoryIndex(element: HTMLElement | null, historyIndex: number | undefined): void {
	if (element && Number.isInteger(historyIndex)) element.dataset.historyIndex = String(historyIndex);
}

function ensureAbortedPartialElement(p: ChatPayload, partialState: AbortedPartialState): HTMLElement | null {
	let element = persistedAssistantMessage(p.messageIndex);
	if (hasNonWhitespaceContent(partialState.partialText)) {
		element ||= S.streamEl || chatAddMsg("assistant", renderMarkdown(partialState.partialText), true);
		if (element && S.streamEl) setSafeMarkdownHtml(element, partialState.partialText);
		assignPartialHistoryIndex(element, p.messageIndex);
		return element;
	}
	if (!hasVisibleReasoning(partialState.partialReasoning)) return element;
	element ||= S.streamEl || chatAddMsg("assistant", "", false);
	assignPartialHistoryIndex(element, p.messageIndex);
	return element;
}

function finalizeAbortedPartial(p: ChatPayload, partialState: AbortedPartialState, partialElement: HTMLElement): void {
	partialElement.classList.remove("reasoning-stream");
	partialElement.querySelector(".thinking-status")?.remove();
	const disclosure = partialElement.querySelector(".msg-reasoning");
	if (hasVisibleReasoning(partialState.partialReasoning)) {
		appendReasoningDisclosure(partialElement, partialState.partialReasoning, { expanded: false, streaming: false });
	} else {
		disclosure?.remove();
	}
	appendMessageActions({
		messageEl: partialElement,
		sessionKey: p.sessionKey || S.activeSessionKey,
		messageIndex: p.messageIndex,
		text: partialState.partialText,
		hasAudio: Boolean(partialState.partial?.audio),
	});
	appendTerminalMetadataForPartial(p, partialState.partial, abortedToolBatchEnd(partialState) || partialElement);
	smartScrollToBottom();
}

export function renderAbortedPartialInDom(p: ChatPayload, partialState: AbortedPartialState): void {
	if (!partialState.hasVisiblePartial) {
		renderMetadataOnlyPartial(p, partialState);
		return;
	}
	const partialElement = ensureAbortedPartialElement(p, partialState);
	if (partialElement) finalizeAbortedPartial(p, partialState, partialElement);
}
