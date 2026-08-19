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
 *
 * Streaming calls this on every delta. The rendered markdown is kept in a
 * dedicated wrapper so the reasoning disclosure and the action bar are never
 * detached, and identical output is not re-applied: replacing unchanged nodes
 * makes the bubble flicker and resets the chat scroll position.
 */
export function setSafeMarkdownHtml(el: HTMLElement, text: string): void {
	const rendered = renderMarkdown(text);
	const body = markdownBody(el);
	if (body.dataset.markdownSource === text) return;
	body.dataset.markdownSource = text;
	body.textContent = "";
	body.insertAdjacentHTML("afterbegin", rendered);
}

/// The wrapper holding rendered markdown inside a message element.
///
/// Created on first use and reused afterwards, so streaming updates touch only
/// its contents and leave the surrounding reasoning disclosure and action bar
/// in place.
function markdownBody(el: HTMLElement): HTMLElement {
	const existing = el.querySelector<HTMLElement>(":scope > .msg-markdown-body");
	if (existing) return existing;
	const body = document.createElement("span");
	body.className = "msg-markdown-body";
	const actionBar = Array.from(el.children).find((child) => child.classList.contains("msg-action-bar"));
	// Legacy inline content from a non-streaming render is replaced by the
	// wrapper, keeping the reasoning disclosure and the action bar attached.
	for (const child of Array.from(el.childNodes)) {
		if (child instanceof HTMLElement && (child.classList.contains("msg-reasoning") || child === actionBar)) {
			continue;
		}
		child.remove();
	}
	if (actionBar) el.insertBefore(body, actionBar);
	else el.appendChild(body);
	return body;
}

export function hasNonWhitespaceContent(text: string | null | undefined): boolean {
	return String(text || "").trim().length > 0;
}
