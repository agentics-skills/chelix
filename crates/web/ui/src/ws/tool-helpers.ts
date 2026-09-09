// ── Tool call utilities ───────────────────────────────────────

import { isA2uiTool, mountA2uiToolCard } from "../a2ui-renderer";
import { chatInsertionTarget, smartScrollToBottom } from "../chat-ui";
import { mountExecuteCommandToolBubble, unmountExecuteCommandToolBubble } from "../components/ExecuteCommandToolBubble";
import { navigate } from "../router";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import {
	appendToolCardContextBudget,
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	isCommandToolName,
	normalizeToolResult,
	renderToolCardError,
	renderToolCardProgress,
	renderToolCardResult,
	setToolCardExpanded,
	setToolCardProgress,
	setToolCardStatus,
	updateToolCardParameters,
} from "../tool-call-card";
import {
	isTerminalToolLifecycle,
	type ToolInvocationSnapshot,
	terminalToolPresentation,
	toolLifecycleArguments,
} from "../tool-lifecycle";
import type { ToolResult } from "../types/ws-events";
import { clearChatEmptyState } from "./shared";

export function toolCallCardId(snapshot: ToolInvocationSnapshot): string {
	return `tool-${snapshot.runId}-${snapshot.lifecycle.toolCallId}`;
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
		renderToolCardResult(toolCard, null, { sessionKey: eventSession || S.activeSessionKey || "main", screenshotMode });
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
			renderToolCardResult(card, null);
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

interface ToolLifecycleRenderOptions {
	renderEarly?: boolean;
	interactive?: boolean;
	screenshotMode?: "inline-base64" | "media";
	assistantId?: string;
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

function updateToolOutput(
	card: HTMLElement,
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
	interactive: boolean,
): void {
	const lifecycle = snapshot.lifecycle;
	if (lifecycle.stage !== "execution_progress") {
		if (isCommandToolName(lifecycle.toolName)) unmountExecuteCommandToolBubble(card);
		renderToolCardProgress(card, null);
		return;
	}
	if (!isCommandToolName(lifecycle.toolName)) {
		renderToolCardProgress(card, lifecycle.message);
		return;
	}
	mountExecuteCommandToolBubble(card, {
		toolCallId: lifecycle.toolCallId,
		sessionKey: eventSession,
		progressMessage: lifecycle.message,
		attachTerminal: interactive && lifecycle.elapsedMs >= 10_000,
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
		interactive: interactive && !presentation,
		success: presentation?.success,
		result: presentation?.result ?? undefined,
		error: presentation?.error ?? undefined,
	});
}

export function renderToolLifecycleSnapshot(
	snapshot: ToolInvocationSnapshot,
	eventSession: string,
	options: ToolLifecycleRenderOptions = {},
): HTMLElement | null {
	const cardId = toolCallCardId(snapshot);
	let card = document.getElementById(cardId) as HTMLElement | null;
	if (!(card || shouldRenderLifecycle(snapshot, options.renderEarly !== false))) return null;
	if (!card) {
		const target = chatInsertionTarget();
		if (!target) return null;
		card = createToolCallCard({
			id: cardId,
			toolCallId: snapshot.lifecycle.toolCallId,
			assistantId: options.assistantId,
			toolName: snapshot.lifecycle.toolName,
			arguments: toolLifecycleArguments(snapshot.lifecycle, snapshot.accumulatedArguments),
			executionMode: snapshot.executionMode,
			status: "running",
			expanded: true,
		});
		clearChatEmptyState();
		target.appendChild(card);
	}
	const renderedSequence = Number(card.dataset.toolSequence);
	if (Number.isSafeInteger(renderedSequence) && renderedSequence >= snapshot.lifecycle.sequence) return card;
	card.dataset.toolSequence = String(snapshot.lifecycle.sequence);
	if (options.assistantId) card.dataset.assistantId = options.assistantId;
	const argumentsValue = toolLifecycleArguments(snapshot.lifecycle, snapshot.accumulatedArguments);
	if (argumentsValue !== undefined) updateToolCardParameters(card, argumentsValue, snapshot.executionMode);
	if (isTerminalToolLifecycle(snapshot.lifecycle)) {
		completeToolCard(card, snapshot, eventSession, options.screenshotMode || "inline-base64");
	} else {
		setToolCardProgress(card, lifecycleStatus(snapshot));
		updateToolOutput(card, snapshot, eventSession, options.interactive !== false);
	}
	updateA2uiSurface(card, snapshot, options.interactive !== false);
	smartScrollToBottom();
	return card;
}
