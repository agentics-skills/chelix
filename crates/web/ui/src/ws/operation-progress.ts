// ── Generic operation progress event handler ─────────────────

import { smartScrollToBottom } from "../chat-ui";
import { currentPrefix } from "../router";
import * as S from "../state";
import type { OperationProgressPayload } from "../types";
import { clearChatEmptyState } from "./shared";

interface OperationIndicator {
	el: HTMLDivElement;
	statusEl: HTMLDivElement;
	progressEl: HTMLDivElement;
	barEl: HTMLDivElement;
	textEl: HTMLDivElement;
	removeTimer: ReturnType<typeof setTimeout> | null;
}

const indicators = new Map<string, OperationIndicator>();

function removeIndicator(operationId: string): void {
	const indicator = indicators.get(operationId);
	if (!indicator) return;
	if (indicator.removeTimer) clearTimeout(indicator.removeTimer);
	indicator.el.remove();
	indicators.delete(operationId);
}

function createIndicator(operationId: string, payload: OperationProgressPayload): OperationIndicator | null {
	if (currentPrefix !== "/chats" || !S.chatMsgBox) return null;

	const el = document.createElement("div");
	el.className = "msg system download-indicator";
	el.dataset.operationId = operationId;

	const statusEl = document.createElement("div");
	statusEl.className = "download-status";
	statusEl.textContent = operationMessage(payload);
	el.appendChild(statusEl);

	const progressEl = document.createElement("div");
	progressEl.className = "download-progress indeterminate";
	const barEl = document.createElement("div");
	barEl.className = "download-progress-bar";
	progressEl.appendChild(barEl);
	el.appendChild(progressEl);

	const textEl = document.createElement("div");
	textEl.className = "download-progress-text";
	textEl.textContent = operationDetail(payload);
	el.appendChild(textEl);

	clearChatEmptyState();
	S.chatMsgBox.appendChild(el);
	smartScrollToBottom();

	return { el, statusEl, progressEl, barEl, textEl, removeTimer: null };
}

function operationMessage(payload: OperationProgressPayload): string {
	if (payload.message) return payload.message;
	if (payload.method === "sessions.reset") return "Resetting session…";
	if (payload.method === "sessions.compact" || payload.method === "chat.compact") return "Compacting context window…";
	return "Operation in progress…";
}

function operationDetail(payload: OperationProgressPayload): string {
	const fraction = progressFraction(payload);
	if (fraction != null) return `${Math.round(fraction * 100)}% complete`;
	return payload.phase || payload.kind || payload.method || "working";
}

function progressFraction(payload: OperationProgressPayload): number | null {
	const current = Number(payload.current);
	const total = Number(payload.total);
	if (!(Number.isFinite(current) && Number.isFinite(total)) || total <= 0) return null;
	return Math.max(0, Math.min(1, current / total));
}

function updateIndicator(operationId: string, indicator: OperationIndicator, payload: OperationProgressPayload): void {
	if (indicator.removeTimer) {
		clearTimeout(indicator.removeTimer);
		indicator.removeTimer = null;
	}

	indicator.statusEl.textContent = operationMessage(payload);
	indicator.textEl.textContent = operationDetail(payload);

	const fraction = progressFraction(payload);
	if (fraction == null) {
		indicator.progressEl.classList.add("indeterminate");
		indicator.barEl.style.width = "";
	} else {
		indicator.progressEl.classList.remove("indeterminate");
		indicator.barEl.style.width = `${Math.round(fraction * 100)}%`;
	}

	if (payload.done) {
		const delayMs = payload.phase === "failed" ? 2_000 : 1_200;
		indicator.removeTimer = setTimeout(() => removeIndicator(operationId), delayMs);
	}
}

export function handleOperationProgress(payload: OperationProgressPayload): void {
	const operationId = payload.operationId;
	if (!operationId) return;

	const sessionKey = payload.sessionKey || undefined;
	if (sessionKey && sessionKey !== S.activeSessionKey) {
		removeIndicator(operationId);
		return;
	}

	let indicator = indicators.get(operationId);
	if (!indicator) {
		indicator = createIndicator(operationId, payload) || undefined;
		if (!indicator) return;
		indicators.set(operationId, indicator);
	}

	updateIndicator(operationId, indicator, payload);
}
