// ── Tool call utilities ───────────────────────────────────────

import { isA2uiTool, mountA2uiToolCard } from "../a2ui-renderer";
import type { ChannelFooterInfo } from "../chat-ui";
import {
	appendChannelFooter,
	appendReasoningDisclosure,
	chatAddMsg,
	chatInsertionTarget,
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
	setToolCardProgress,
	setToolCardStatus,
	toolCallIds,
	updateToolCardParameters,
} from "../tool-call-card";
import {
	isTerminalToolLifecycle,
	reduceToolInvocation,
	type ToolInvocationSnapshot,
	terminalToolPresentation,
	toolInvocationKey,
	toolLifecycleArguments,
} from "../tool-lifecycle";
import {
	type AbortedPartialState,
	type ChatPayload,
	hasVisibleReasoning,
	type ToolLifecyclePayload,
	type ToolResult,
} from "../types/ws-events";
import { clearLiveSegment, currentLiveSegment, liveFunctionCallPosition } from "./live-segments";
import { placeSegmentNode, SegmentPlacementError } from "./segment-placement";
import { clearChatEmptyState, hasNonWhitespaceContent, setSafeMarkdownHtml } from "./shared";

// ── Tool lifecycle snapshot tracking ──────────────────────────

const liveToolInvocations = new Map<string, ToolInvocationSnapshot>();

export function toolCallCardId(snapshot: ToolInvocationSnapshot): string {
	const { toolCallId } = snapshot.lifecycle;
	return snapshot.runId ? `tool-${snapshot.runId}-${toolCallId}` : `tool-${toolCallId}`;
}

export function reduceLiveToolInvocation(eventSession: string, payload: ToolLifecyclePayload): ToolInvocationSnapshot {
	const key = toolInvocationKey(eventSession, payload.runId, payload.toolCallId);
	const snapshot = reduceToolInvocation(liveToolInvocations.get(key), payload, {
		runId: payload.runId,
		executionMode: payload.executionMode,
		messageIndex: payload.messageIndex,
		assistantMessage: payload.assistantMessage,
		assistantMessageIndex: payload.assistantMessageIndex,
		contextBudget: payload.contextBudget,
	});
	liveToolInvocations.set(key, snapshot);
	return snapshot;
}

export function clearToolLifecycleStateForSession(sessionKey: string): void {
	const prefix = `${sessionKey}:`;
	for (const key of liveToolInvocations.keys()) {
		if (key.startsWith(prefix)) liveToolInvocations.delete(key);
	}
	// The streaming segment ends with its tool lifecycles; keeping it would let
	// a stale segment position nodes of the next run.
	clearLiveSegment(sessionKey);
}

// ── Tool result rendering ─────────────────────────────────────

function appendToolResult(
	toolCard: HTMLElement,
	resultValue: ToolResult | string,
	eventSession: string,
	screenshotMode: "inline-base64" | "media",
): void {
	const result = normalizeToolResult(resultValue);
	const out = (result.stdout || result.output || "").replace(/\n+$/, "");
	const toolSession = sessionStore.getByKey(eventSession);
	if (toolSession) toolSession.lastToolOutput.value = out;
	S.setLastToolOutput(out);
	renderToolCardResult(toolCard, result, {
		sessionKey: eventSession || S.activeSessionKey || "main",
		screenshotMode,
	});
}

function appendSkillChangeHint(toolCard: HTMLElement, snapshot: ToolInvocationSnapshot, success: boolean): void {
	const toolName = snapshot.lifecycle.toolName;
	if (!success || (toolName !== "create_skill" && toolName !== "update_skill")) return;
	const hint = document.createElement("div");
	hint.className = "skill-hint";
	const verb = toolName === "create_skill" ? "created" : "updated";
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

function completeToolCard(
	toolCard: HTMLElement,
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
	screenshotMode: "inline-base64" | "media",
): void {
	const presentation = terminalToolPresentation(snapshot.lifecycle);
	if (!presentation) return;
	unmountExecuteCommandToolBubble(toolCard);
	setToolCardStatus(toolCard, presentation.rejected ? "retry" : presentation.success ? "success" : "error");
	if (presentation.result !== null) {
		appendToolResult(toolCard, presentation.result, eventSession, screenshotMode);
		if (!presentation.success && presentation.error) {
			appendToolCardError(toolCard, presentation.error, presentation.rejected);
		}
	} else if (presentation.success) {
		renderToolCardResult(toolCard, {}, { sessionKey: eventSession || S.activeSessionKey || "main", screenshotMode });
	} else {
		renderToolCardError(toolCard, presentation.error || undefined, presentation.rejected);
	}
	appendToolCardContextBudget(toolCard, snapshot.contextBudget);
	setToolCardExpanded(
		toolCard,
		presentation.rejected || isCommandToolName(snapshot.lifecycle.toolName) || isA2uiTool(snapshot.lifecycle.toolName),
	);
	appendSkillChangeHint(toolCard, snapshot, presentation.success);
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

// ── Canonical assistant segment ───────────────────────────────

/** Close the live assistant segment when input-ready supplies its persisted frame.
 *
 * Binding the live element to the canonical assistant history index keeps the
 * next iteration's deltas below the invocation card.
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

interface ToolLifecycleRenderOptions {
	renderEarly?: boolean;
	interactive?: boolean;
	screenshotMode?: "inline-base64" | "media";
	assistantHistoryIndex?: number;
}

function lifecycleStatus(snapshot: ToolInvocationSnapshot): string {
	switch (snapshot.lifecycle.stage) {
		case "created":
			return "preparing…";
		case "input_streaming":
			return "receiving parameters…";
		case "input_ready":
			return "parameters ready";
		case "waiting_for_execution":
			return "waiting for execution…";
		case "executing":
			return "running…";
		case "execution_progress":
			return snapshot.lifecycle.message;
		case "result_ready":
			return "result ready";
		case "completed":
			return snapshot.lifecycle.success ? "completed" : "failed";
		case "rejected":
			return "needs retry";
		case "cancelled":
			return "cancelled";
	}
}

function shouldRenderLifecycle(snapshot: ToolInvocationSnapshot, renderEarly: boolean): boolean {
	return renderEarly || (snapshot.lifecycle.stage !== "created" && snapshot.lifecycle.stage !== "input_streaming");
}

function updateExecuteCommandBubble(
	card: HTMLElement,
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
	interactive: boolean,
): void {
	const lifecycle = snapshot.lifecycle;
	if (!isCommandToolName(lifecycle.toolName) || isTerminalToolLifecycle(lifecycle)) return;
	mountExecuteCommandToolBubble(card, {
		toolCallId: lifecycle.toolCallId,
		sessionKey: eventSession,
		progressMessage: lifecycleStatus(snapshot),
		attachTerminal: interactive && lifecycle.stage === "execution_progress" && lifecycle.elapsedMs >= 10_000,
	});
}

function updateA2uiSurface(card: HTMLElement, snapshot: ToolInvocationSnapshot, interactive: boolean): void {
	const lifecycle = snapshot.lifecycle;
	if (!isA2uiTool(lifecycle.toolName) || lifecycle.stage === "rejected") return;
	const argumentsValue = toolLifecycleArguments(lifecycle, snapshot.accumulatedArguments);
	if (!(argumentsValue && typeof argumentsValue === "object" && !Array.isArray(argumentsValue))) return;
	const presentation = terminalToolPresentation(lifecycle);
	mountA2uiToolCard(card, {
		arguments: argumentsValue,
		runId: snapshot.runId,
		toolCallId: lifecycle.toolCallId,
		interactive,
		success: presentation?.success,
		result: presentation?.result ?? undefined,
		error: presentation?.error ?? undefined,
	});
}

function closeCanonicalAssistantBeforeCard(snapshot: ToolInvocationSnapshot, eventSession: string): void {
	if (snapshot.lifecycle.stage !== "input_ready" || !snapshot.assistantMessage) return;
	// Replayed records describe a finished turn. The live element belongs to the
	// run streaming right now, and closing it from a replay would cut off the
	// response the user is watching.
	if (S.chatBatchLoading) return;
	// `input_ready` repeats for every tool call of the same assistant turn, but
	// the bubble is closed once. Reapplying the canonical frame afterwards would
	// repaint an already finished bubble on every following tool call.
	if (!S.streamEl && assistantSegmentByHistoryIndex(snapshot.assistantMessageIndex)) return;
	closeLiveAssistantSegment(snapshot.assistantMessage, snapshot.assistantMessageIndex, eventSession);
}

/// Insert a freshly created tool card at the canonical position of its
/// function call inside the streaming segment.
///
/// Without a streaming segment there is nothing to order against: history
/// replay and session switching render cards outside any live segment, so the
/// card is appended. Inside a segment the call must be known, and a card whose
/// call the segment never announced is reported as a defect.
function placeToolCard(
	container: HTMLElement,
	card: HTMLElement,
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
): void {
	// A batch replay carries its own order: the records are rendered in history
	// order. The live segment describes the run currently streaming, which is a
	// different part of the conversation, so it must not position these cards.
	if (S.chatBatchLoading) {
		container.appendChild(card);
		return;
	}
	const segment = currentLiveSegment(eventSession);
	if (!segment) {
		container.appendChild(card);
		return;
	}
	const position = liveFunctionCallPosition(eventSession, snapshot.lifecycle.toolCallId);
	if (position === null) {
		container.appendChild(card);
		throw new SegmentPlacementError(
			`tool call \`${snapshot.lifecycle.toolCallId}\` has no provider item in segment \`${segment.segmentId}\``,
		);
	}
	placeSegmentNode(container, card, segment.segmentId, position);
}

export function renderToolLifecycleSnapshot(
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
	options: ToolLifecycleRenderOptions = {},
): HTMLElement | null {
	const cardId = toolCallCardId(snapshot);
	let card = document.getElementById(cardId) as HTMLElement | null;
	closeCanonicalAssistantBeforeCard(snapshot, eventSession);
	if (!(card || shouldRenderLifecycle(snapshot, options.renderEarly !== false))) return null;
	if (!card) {
		const target = chatInsertionTarget();
		if (!target) return null;
		card = createToolCallCard({
			id: cardId,
			toolCallId: snapshot.lifecycle.toolCallId,
			assistantHistoryIndex: options.assistantHistoryIndex ?? snapshot.assistantMessageIndex,
			toolName: snapshot.lifecycle.toolName,
			arguments: toolLifecycleArguments(snapshot.lifecycle, snapshot.accumulatedArguments),
			executionMode: snapshot.executionMode,
			status: "running",
			expanded: true,
		});
		clearChatEmptyState();
		placeToolCard(target, card, snapshot, eventSession);
	}
	const renderedSequence = Number(card.dataset.toolSequence);
	if (Number.isSafeInteger(renderedSequence) && renderedSequence >= snapshot.lifecycle.sequence) return card;
	card.dataset.toolSequence = String(snapshot.lifecycle.sequence);
	const assistantHistoryIndex = options.assistantHistoryIndex ?? snapshot.assistantMessageIndex;
	if (Number.isInteger(assistantHistoryIndex)) card.dataset.assistantHistoryIndex = String(assistantHistoryIndex);
	const argumentsValue = toolLifecycleArguments(snapshot.lifecycle, snapshot.accumulatedArguments);
	if (argumentsValue !== undefined) updateToolCardParameters(card, argumentsValue, snapshot.executionMode);
	if (isTerminalToolLifecycle(snapshot.lifecycle)) {
		completeToolCard(card, snapshot, eventSession, options.screenshotMode || "inline-base64");
	} else {
		setToolCardProgress(card, lifecycleStatus(snapshot));
		updateExecuteCommandBubble(card, snapshot, eventSession, options.interactive !== false);
	}
	updateA2uiSurface(card, snapshot, options.interactive !== false);
	smartScrollToBottom();
	return card;
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
