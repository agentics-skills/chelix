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
import {
	formatAssistantTokenUsage,
	formatTokenSpeed,
	renderAudioPlayer,
	renderMarkdown,
	tokenSpeedTone,
} from "../helpers";
import { appendMessageActions } from "../message-actions";
import { navigate } from "../router";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import {
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	renderToolCardError,
	renderToolCardResult,
	setToolCardExpanded,
	setToolCardStatus,
} from "../tool-call-card";
import type { ChatPayload, ToolCallPayload, ToolResult } from "../types/ws-events";
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
	setToolCardExpanded(toolCard, p.toolName === "exec");

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
	const statusEls = S.chatMsgBox.querySelectorAll(".msg.exec-card .exec-status");
	for (const statusEl of statusEls) {
		const card = statusEl.closest(".msg.exec-card") as HTMLElement | null;
		if (!card) continue;
		if (!card.classList.contains("running")) continue;
		if (card.classList.contains("tool-call-card")) {
			setToolCardStatus(card, "success");
			setToolCardExpanded(card, false);
			continue;
		}
		statusEl.remove();
		if (!(card.classList.contains("exec-ok") || card.classList.contains("exec-err"))) {
			card.className = "msg exec-card exec-ok";
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
	// Close the current streaming element so new text deltas after this tool
	// call will create a fresh element positioned after the tool card
	if (S.streamEl) {
		// Remove the element if it's empty (e.g. only whitespace from a
		// pre-tool-call delta) to avoid leaving an orphaned empty div.
		if (!S.streamEl.textContent?.trim()) {
			S.streamEl.remove();
		}
		S.setStreamEl(null);
		S.setStreamText("");
	}
	const cardId = toolCallCardId(p);
	if (document.getElementById(cardId)) return;
	const card = createToolCallCard({
		id: cardId,
		toolName: p.toolName,
		arguments: p.arguments,
		executionMode: p.executionMode,
		status: "running",
		expanded: true,
	});
	// Preserve thinking text as a reasoning disclosure inside the tool card
	if (thinkingText) appendReasoningDisclosure(getToolCardDetailsContainer(card), thinkingText);
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
		if (hasFinalText) return chatAddMsg("assistant", renderMarkdown(finalText), true);
		// No text (silent reply) -- remove any leftover stream element.
		if (S.streamEl) S.streamEl.remove();
		return null;
	}
	if (S.streamEl) S.streamEl.remove();
	return null;
}

// ── Final footer ──────────────────────────────────────────────

export function appendFinalFooter(msgEl: HTMLElement | null, p: ChatPayload, eventSession: string): void {
	if (!(msgEl && p.model)) return;
	const footer = document.createElement("div");
	footer.className = "msg-model-footer";
	let footerText = p.provider ? `${p.provider} / ${p.model}` : p.model;
	if (p.reasoningEffort !== undefined) {
		footerText += ` \u00b7 reasoning_effort: ${p.reasoningEffort || "off"}`;
	}
	if (p.inputTokens || p.outputTokens) {
		footerText += ` \u00b7 ${formatAssistantTokenUsage(p.inputTokens, p.outputTokens, p.cacheReadTokens)}`;
	}
	const textSpan = document.createElement("span");
	textSpan.textContent = footerText;
	footer.appendChild(textSpan);

	const speedLabel = formatTokenSpeed(p.outputTokens || 0, p.durationMs || 0);
	if (speedLabel) {
		const speed = document.createElement("span");
		speed.className = "msg-token-speed";
		const tone = tokenSpeedTone(p.outputTokens || 0, p.durationMs || 0);
		if (tone) speed.classList.add(`msg-token-speed-${tone}`);
		speed.textContent = ` \u00b7 ${speedLabel}`;
		footer.appendChild(speed);
	}

	if (p.replyMedium === "voice" || p.replyMedium === "text") {
		const badge = document.createElement("span");
		badge.className = "reply-medium-badge";
		badge.textContent = p.replyMedium;
		footer.appendChild(badge);
	}
	msgEl.appendChild(footer);

	appendMessageActions({
		messageEl: msgEl,
		sessionKey: p.sessionKey || eventSession || S.activeSessionKey,
		messageIndex: p.messageIndex,
		text: p.text || "",
		runId: p.runId,
		hasAudio: !!p.audio,
		audioWarning: p.audioWarning || undefined,
	});
}

// ── Aborted partial rendering ─────────────────────────────────

export function renderAbortedPartialInDom(
	eventSession: string,
	p: ChatPayload,
	partialState: {
		partial: ChatPayload["partialMessage"] | null;
		partialText: string;
		partialReasoning: string;
		hasVisiblePartial: boolean;
	},
): void {
	if (!partialState.hasVisiblePartial) return;
	const partial = partialState.partial;
	let partialEl: HTMLElement | null = null;
	if (hasNonWhitespaceContent(partialState.partialText) && S.streamEl) {
		setSafeMarkdownHtml(S.streamEl, partialState.partialText);
		partialEl = S.streamEl;
	} else if (hasNonWhitespaceContent(partialState.partialText)) {
		partialEl = chatAddMsg("assistant", renderMarkdown(partialState.partialText), true);
	} else if (hasNonWhitespaceContent(partialState.partialReasoning)) {
		partialEl = chatAddMsg("assistant", "", false);
	}
	if (partialEl && partialState.partialReasoning && !isReasoningAlreadyShown(partialState.partialReasoning)) {
		appendReasoningDisclosure(partialEl, partialState.partialReasoning);
	}
	if (!partialEl) return;
	appendFinalFooter(
		partialEl,
		{
			model: partial?.model || "",
			provider: partial?.provider || "",
			inputTokens: partial?.inputTokens || 0,
			outputTokens: partial?.outputTokens || 0,
			durationMs: partial?.durationMs || 0,
			replyMedium: p.replyMedium || "text",
			text: partialState.partialText,
			audio: partial?.audio || undefined,
			audioWarning: undefined,
			runId: p.runId,
			messageIndex: p.messageIndex,
			sessionKey: eventSession,
		},
		eventSession,
	);
	smartScrollToBottom();
}
