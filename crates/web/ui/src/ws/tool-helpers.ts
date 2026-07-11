// ── Tool call utilities ───────────────────────────────────────

import type { ChannelFooterInfo } from "../chat-ui";
import {
	appendChannelFooter,
	appendReasoningDisclosure,
	chatAddMsg,
	removeThinking,
	smartScrollToBottom,
	stripChannelPrefix,
} from "../chat-ui";
import { renderAudioPlayer, renderMarkdown } from "../helpers";
import { navigate } from "../router";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import {
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	isCommandToolName,
	renderToolCardError,
	renderToolCardResult,
	resolveToolBatchEnd,
	setToolCardExpanded,
	setToolCardStatus,
	toolCallIds,
} from "../tool-call-card";
import type { AbortedPartialState, ChatPayload, ToolCallPayload, ToolResult } from "../types/ws-events";
import { clearChatEmptyState, hasNonWhitespaceContent, isReasoningAlreadyShown, setSafeMarkdownHtml } from "./shared";

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
	clearChatEmptyState();
	S.chatMsgBox.appendChild(card);
	smartScrollToBottom();
	return card;
}

// ── Tool result rendering ─────────────────────────────────────

export function appendToolResult(toolCard: HTMLElement, result: ToolResult, eventSession: string): void {
	const out = (result.stdout || "").replace(/\n+$/, "");
	// Update per-session signal
	const toolSession = sessionStore.getByKey(eventSession);
	if (toolSession) toolSession.lastToolOutput.value = out;
	// Dual-write to global state for backward compat
	S.setLastToolOutput(out);
	renderToolCardResult(toolCard, result, { sessionKey: eventSession || S.activeSessionKey || "main" });
}

// ── Tool card completion ──────────────────────────────────────

function isToolValidationErrorPayload(p: ChatPayload): boolean {
	if (!(p && !p.success && p.error && p.error.detail)) return false;
	const errDetail = p.error.detail.toLowerCase();
	return (
		errDetail.includes("missing field") ||
		errDetail.includes("missing required") ||
		errDetail.includes("missing 'action'") ||
		errDetail.includes("missing 'url'")
	);
}

export function completeToolCard(toolCard: HTMLElement, p: ChatPayload, eventSession: string): void {
	// Use muted "retry" style for validation errors, normal styles otherwise.
	if (isToolValidationErrorPayload(p)) {
		setToolCardStatus(toolCard, "retry");
	} else {
		setToolCardStatus(toolCard, p.success ? "success" : "error");
	}

	if (p.success && p.result) {
		appendToolResult(toolCard, p.result, eventSession);
	} else if (!p.success && p.result) {
		appendToolResult(toolCard, p.result, eventSession);
		if (p.error) appendToolCardError(toolCard, p.error, isToolValidationErrorPayload(p));
	} else if (p.success) {
		renderToolCardResult(toolCard, {}, { sessionKey: eventSession || S.activeSessionKey || "main" });
	} else if (p.error) {
		renderToolCardError(toolCard, p.error, isToolValidationErrorPayload(p));
	}
	setToolCardExpanded(toolCard, isCommandToolName(p.toolName));

	// Show a hint below the card when a skill is created or updated.
	if (p.success && (p.toolName === "create_skill" || p.toolName === "update_skill")) {
		const hint = document.createElement("div");
		hint.className = "skill-hint";
		const verb = p.toolName === "create_skill" ? "created" : "updated";
		const link = document.createElement("a");
		link.href = "/skills";
		link.textContent = "personal skills";
		link.addEventListener("click", (e: MouseEvent) => {
			e.preventDefault();
			navigate("/skills");
		});
		hint.append(`Skill ${verb} \u2014 available in your `, link);
		getToolCardDetailsContainer(toolCard).appendChild(hint);
	}
}

export function clearStaleRunningToolCards(): void {
	if (!S.chatMsgBox) return;
	const statusEls = S.chatMsgBox.querySelectorAll(".msg.command-card .command-status");
	for (const statusEl of statusEls) {
		const card = statusEl.closest(".msg.command-card") as HTMLElement | null;
		if (!card) continue;
		if (!card.classList.contains("running")) continue;
		if (card.classList.contains("tool-call-card")) {
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

// ── Tool call start (with thinking text extraction) ───────────

/** Extract thinking text from the indicator before it is removed. Returns the
 * trimmed text or null if the indicator has no thinking content. */
function extractThinkingText(): string | null {
	const indicator = document.getElementById("thinkingIndicator");
	if (!indicator) return null;
	const textEl = indicator.querySelector(".thinking-text");
	const text = textEl?.textContent?.trim();
	return text || null;
}

export function handleToolCallStartDom(p: ChatPayload, eventSession: string): void {
	const thinkingText = extractThinkingText();
	removeThinking();
	const canonical = p.assistantMessage;
	const canonicalText = canonical?.content || "";
	const canonicalReasoning = canonical?.reasoning || "";
	// The server persisted the assistant segment before this tool event. Bind
	// the live element to its canonical history index, then start a fresh
	// segment for post-tool deltas.
	if (S.streamEl) {
		if (hasNonWhitespaceContent(canonicalText) || hasNonWhitespaceContent(canonicalReasoning)) {
			if (hasNonWhitespaceContent(canonicalText)) {
				setSafeMarkdownHtml(S.streamEl, canonicalText);
			}
			if (Number.isInteger(p.messageIndex)) {
				S.streamEl.dataset.historyIndex = String(p.messageIndex);
			}
			if (canonicalReasoning && !isReasoningAlreadyShown(canonicalReasoning)) {
				appendReasoningDisclosure(S.streamEl, canonicalReasoning);
			}
		} else {
			S.streamEl.remove();
		}
		S.setStreamEl(null);
		S.setStreamText("");
	} else if (hasNonWhitespaceContent(canonicalText) || hasNonWhitespaceContent(canonicalReasoning)) {
		const existingSegment = Number.isInteger(p.messageIndex)
			? (S.chatMsgBox?.querySelector(`.msg.assistant[data-history-index="${p.messageIndex}"]`) as HTMLElement | null)
			: null;
		const segment = existingSegment || chatAddMsg("assistant", renderMarkdown(canonicalText), true);
		if (segment && Number.isInteger(p.messageIndex)) {
			segment.dataset.historyIndex = String(p.messageIndex);
		}
		if (segment && canonicalReasoning && !isReasoningAlreadyShown(canonicalReasoning)) {
			appendReasoningDisclosure(segment, canonicalReasoning);
		}
	}
	const cardId = toolCallCardId(p);
	const existingCard = document.getElementById(cardId) as HTMLElement | null;
	if (existingCard) {
		if (Number.isInteger(p.messageIndex)) {
			existingCard.dataset.assistantHistoryIndex = String(p.messageIndex);
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
	// Preserve thinking text as a reasoning disclosure inside the tool card
	if (thinkingText && !canonicalReasoning) {
		appendReasoningDisclosure(getToolCardDetailsContainer(card), thinkingText);
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

	const cleanText = stripChannelPrefix(p.text || "");
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

export function resolveFinalMessageEl(p: ChatPayload): HTMLElement | null {
	const finalText = String(p.text || "");
	const hasFinalText = hasNonWhitespaceContent(finalText);
	const isEcho = hasFinalText && isPureToolOutputEcho(finalText, S.lastToolOutput);
	if (!isEcho) {
		if (hasFinalText && S.streamEl) {
			setSafeMarkdownHtml(S.streamEl, finalText);
			return S.streamEl;
		}
		if (hasFinalText) {
			if (Number.isInteger(p.messageIndex)) {
				const persisted = S.chatMsgBox?.querySelector(
					`.msg.assistant[data-history-index="${p.messageIndex}"]`,
				) as HTMLElement | null;
				if (persisted) return persisted;
			}
			return chatAddMsg("assistant", renderMarkdown(finalText), true);
		}
		// No text (silent reply) -- remove any leftover stream element.
		if (S.streamEl) S.streamEl.remove();
		return null;
	}
	if (S.streamEl) S.streamEl.remove();
	return null;
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
export function renderAbortedPartialInDom(p: ChatPayload, partialState: AbortedPartialState): void {
	const partial = partialState.partial;
	if (!partialState.hasVisiblePartial) {
		const toolBatchEnd = partialState.hasTerminalToolBatch
			? resolveToolBatchEnd(toolCallIds(partial?.tool_calls))
			: null;
		if (toolBatchEnd && appendTerminalMetadataForPartial(p, partial, toolBatchEnd)) {
			smartScrollToBottom();
		}
		return;
	}
	let partialEl = Number.isInteger(p.messageIndex)
		? (S.chatMsgBox?.querySelector(`.msg.assistant[data-history-index="${p.messageIndex}"]`) as HTMLElement | null)
		: null;
	if (hasNonWhitespaceContent(partialState.partialText)) {
		partialEl ||= S.streamEl || chatAddMsg("assistant", renderMarkdown(partialState.partialText), true);
		if (partialEl && S.streamEl) setSafeMarkdownHtml(partialEl, partialState.partialText);
		if (partialEl && Number.isInteger(p.messageIndex)) {
			partialEl.dataset.historyIndex = String(p.messageIndex);
		}
	} else if (hasNonWhitespaceContent(partialState.partialReasoning)) {
		partialEl ||= chatAddMsg("assistant", "", false);
		if (partialEl && Number.isInteger(p.messageIndex)) {
			partialEl.dataset.historyIndex = String(p.messageIndex);
		}
	}
	if (partialEl && partialState.partialReasoning && !isReasoningAlreadyShown(partialState.partialReasoning)) {
		appendReasoningDisclosure(partialEl, partialState.partialReasoning);
	}
	if (!partialEl) return;
	const toolBatchEnd = partialState.hasTerminalToolBatch ? resolveToolBatchEnd(toolCallIds(partial?.tool_calls)) : null;
	appendTerminalMetadataForPartial(p, partial, toolBatchEnd || partialEl);
	smartScrollToBottom();
}
