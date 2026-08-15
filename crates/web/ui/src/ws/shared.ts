// ── Shared helpers used across ws/ sub-modules ───────────────

import { renderMarkdown } from "../helpers";
import { setSessionActiveRunId } from "../sessions";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";

// ── Chat empty-state management ───────────────────────────────

export function clearChatEmptyState(): void {
	if (!S.chatMsgBox) return;
	const welcome = S.chatMsgBox.querySelector("#welcomeCard");
	if (welcome) welcome.remove();
	const noProviders = S.chatMsgBox.querySelector("#noProvidersCard");
	if (noProviders) noProviders.remove();
	S.chatMsgBox.classList.remove("chat-messages-empty");
}

// ── Session helpers ───────────────────────────────────────────

export function updateSessionRunId(sessionKey: string, runId: string | undefined): void {
	if (!runId) return;
	setSessionActiveRunId(sessionKey, runId);
}

export function updateSessionHistoryIndex(sessionKey: string, messageIndex: number | undefined): void {
	const idx = Number(messageIndex);
	if (!Number.isInteger(idx) || idx < 0) return;
	const session = sessionStore.getByKey(sessionKey);
	if (session && idx > session.lastHistoryIndex.value) {
		session.lastHistoryIndex.value = idx;
	}
	if (sessionKey === sessionStore.activeSessionKey.value && idx > S.lastHistoryIndex) {
		S.setLastHistoryIndex(idx);
	}
}

// ── Markdown rendering ────────────────────────────────────────

/**
 * Safe wrapper: renderMarkdown uses the `marked` library which HTML-escapes
 * all input by default. No raw user content reaches innerHTML.
 */
export function setSafeMarkdownHtml(el: HTMLElement, text: string): void {
	const rendered = renderMarkdown(text);
	const reasoning = Array.from(el.children).find((child) => child.classList.contains("msg-reasoning"));
	const actionBar = Array.from(el.children).find((child) => child.classList.contains("msg-action-bar"));
	if (reasoning) reasoning.remove();
	if (actionBar) actionBar.remove();
	el.textContent = "";
	const wrapper = document.createElement("span");
	wrapper.insertAdjacentHTML("afterbegin", rendered);
	while (wrapper.firstChild) el.appendChild(wrapper.firstChild);
	if (reasoning) el.prepend(reasoning);
	if (actionBar) el.appendChild(actionBar);
}

export function hasNonWhitespaceContent(text: string | null | undefined): boolean {
	return String(text || "").trim().length > 0;
}
