import { chatAddMsg } from "../chat-ui";
import { replaceQueuedPromptsDock } from "../pages/chat/prompt-queue";
import { setSessionActiveRunId } from "../sessions";
import * as S from "../state";
import type { ChatSendPayload } from "../types/chat";
import type { RpcResponse } from "../types/rpc";

interface PendingSend {
	sessionKey: string;
	element: HTMLElement | null;
}

const pending = new Map<string, PendingSend>();

export function registerPendingSend(sessionKey: string, element: HTMLElement | null): string {
	const id = crypto.randomUUID();
	if (element) element.dataset.clientMessageId = id;
	pending.set(id, { sessionKey, element });
	return id;
}

export function confirmPendingSend(sessionKey: string, clientMessageId: unknown): HTMLElement | null {
	if (typeof clientMessageId !== "string") return null;
	const entry = pending.get(clientMessageId);
	if (entry?.sessionKey !== sessionKey) return null;
	pending.delete(clientMessageId);
	if (entry.element) delete entry.element.dataset.clientMessageId;
	return entry.element;
}

export function rejectPendingSend(sessionKey: string, id: string, message?: string): void {
	const entry = pending.get(id);
	if (entry?.sessionKey === sessionKey) {
		entry.element?.remove();
		pending.delete(id);
	}
	if (message && sessionKey === S.activeSessionKey) chatAddMsg("error", message);
}

export function handlePendingSendResponse(
	sessionKey: string,
	id: string,
	response: RpcResponse<ChatSendPayload>,
): void {
	if (!response.ok) {
		rejectPendingSend(sessionKey, id, response.error?.message || "Request failed");
		return;
	}
	if (response.payload?.runId) setSessionActiveRunId(sessionKey, response.payload.runId);
	if (response.payload?.queued) {
		rejectPendingSend(sessionKey, id);
		if (sessionKey === S.activeSessionKey) replaceQueuedPromptsDock(response.payload.status);
	}
}

export function clearPendingSends(sessionKey?: string): void {
	for (const [id, entry] of pending) {
		if (sessionKey !== undefined && entry.sessionKey !== sessionKey) continue;
		entry.element?.remove();
		pending.delete(id);
	}
}
