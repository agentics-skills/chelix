// ── Chat UI ─────────────────────────────────────────────────

import { unmountExecuteCommandToolBubbles } from "./components/ExecuteCommandToolBubble";
import { formatTokens, parseErrorMessage, renderDocument, renderMarkdown, sendRpc } from "./helpers";
import * as S from "./state";
import type { ContextBudgetMetadata, ReasoningContent } from "./types/ws-events";

interface ErrorCardData {
	icon?: string;
	title: string;
	detail?: string;
	provider?: string;
}

interface ImageAttachment {
	dataUrl: string;
	name: string;
}

export interface DocumentAttachment {
	display_name: string;
	stored_filename: string;
	mime_type: string;
	size_bytes?: number;
	url?: string;
}

function clearChatEmptyState(): void {
	if (!S.chatMsgBox) return;
	const welcome = S.chatMsgBox.querySelector("#welcomeCard");
	if (welcome) welcome.remove();
	const noProviders = S.chatMsgBox.querySelector("#noProvidersCard");
	if (noProviders) noProviders.remove();
	S.chatMsgBox.classList.remove("chat-messages-empty");
}

// Scroll state for rAF-based auto-scroll — prevents re-entrancy during streaming
let isAutoScrolling = false;
let shouldFollowChat = true;
let trackedChatMsgBox: HTMLElement | null = null;
// Container new chat nodes are inserted into. It is the chat box itself except
// while an older history page is rendered into a detached fragment, so the
// page can be built without touching what is already on screen.
let chatInsertTarget: HTMLElement | null = null;
// `scrollTop` written by the last programmatic scroll, used to tell our own
// scroll events apart from the user's.
let programmaticScrollTop: number | null = null;

/// Container that receives newly rendered chat nodes.
export function chatInsertionTarget(): HTMLElement | null {
	return chatInsertTarget ?? S.chatMsgBox;
}

/// Render everything `build` produces into `target` instead of the chat box.
///
/// The target is restored afterwards, including when `build` throws: a render
/// that failed halfway must not leave later messages writing into a detached
/// node.
export function withChatInsertionTarget(target: HTMLElement, build: () => void): void {
	const previous = chatInsertTarget;
	chatInsertTarget = target;
	try {
		build();
	} finally {
		chatInsertTarget = previous;
	}
}

function handleChatScroll(): void {
	if (!S.chatMsgBox) return;
	// The scroll event produced by our own scroll carries the exact position we
	// wrote. Every other event is real user intent and must be able to stop the
	// chat from following the stream — during streaming a pending animation
	// frame would otherwise swallow all of them.
	if (programmaticScrollTop !== null && S.chatMsgBox.scrollTop === programmaticScrollTop) {
		programmaticScrollTop = null;
		return;
	}
	programmaticScrollTop = null;
	shouldFollowChat = isChatAtBottom();
	if (shouldFollowChat) hideNewContentIndicator();
}

function handleChatMediaLoad(): void {
	// An image that finishes loading grows the chat below the pinned bottom.
	// While the chat is following, the bottom must stay the bottom; a user
	// reading older messages is not moved.
	scrollChatToBottom();
}

function ensureChatFollowTracking(): void {
	if (!S.chatMsgBox || trackedChatMsgBox === S.chatMsgBox) return;
	if (trackedChatMsgBox) {
		trackedChatMsgBox.removeEventListener("scroll", handleChatScroll);
		trackedChatMsgBox.removeEventListener("load", handleChatMediaLoad, true);
	}
	trackedChatMsgBox = S.chatMsgBox;
	shouldFollowChat = true;
	trackedChatMsgBox.addEventListener("scroll", handleChatScroll, { passive: true });
	// `load` does not bubble, but the capture phase still visits ancestors, so
	// one listener covers every image the chat will ever contain.
	trackedChatMsgBox.addEventListener("load", handleChatMediaLoad, true);
}

export function syncChatFollowStateFromPosition(): void {
	ensureChatFollowTracking();
	shouldFollowChat = isChatAtBottom();
	if (shouldFollowChat) hideNewContentIndicator();
}

// Scroll chat to bottom using requestAnimationFrame to sync with browser paint cycle.
// When force=false (default), follows only while the user has not intentionally
// scrolled up. Pass force=true for imperative scrolls (indicator click,
// autoScrollMode "always", user-sent messages).
export function scrollChatToBottom(force = false): void {
	if (!S.chatMsgBox) return;
	// A batch render inserts many nodes at once. Scrolling per node would fight
	// the position the batch is about to establish, so the batch scrolls once,
	// after it finished.
	if (S.chatBatchLoading) return;
	ensureChatFollowTracking();
	if (force) shouldFollowChat = true;
	else if (!shouldFollowChat) return;
	// A frame is already pending; it will scroll to the height reached by then.
	if (isAutoScrolling) return;
	isAutoScrolling = true;

	requestAnimationFrame(() => {
		isAutoScrolling = false;
		if (!S.chatMsgBox) return;
		// The user may have scrolled away while the frame was pending. Their
		// position wins: the chat must not snap back to the bottom.
		if (!shouldFollowChat) return;
		S.chatMsgBox.scrollTop = S.chatMsgBox.scrollHeight;
		programmaticScrollTop = S.chatMsgBox.scrollTop;
		hideNewContentIndicator();
	});
}

/// Set the chat scroll position to the bottom synchronously.
///
/// A history render must establish its final position before follow-up work
/// measures the viewport (headroom fill), so it cannot wait for an animation
/// frame the way streaming scrolls do. `force=true` resets follow mode,
/// exactly like `scrollChatToBottom(true)`.
export function pinChatToBottom(force = false): void {
	if (!S.chatMsgBox) return;
	ensureChatFollowTracking();
	if (force) shouldFollowChat = true;
	else if (!shouldFollowChat) return;
	S.chatMsgBox.scrollTop = S.chatMsgBox.scrollHeight;
	programmaticScrollTop = S.chatMsgBox.scrollTop;
	hideNewContentIndicator();
}

/** Returns true when the chat scroll position is within `threshold` px of the bottom. */
export function isChatAtBottom(threshold = 60): boolean {
	if (!S.chatMsgBox) return true;
	const { scrollTop, scrollHeight, clientHeight } = S.chatMsgBox;
	return scrollHeight - scrollTop - clientHeight < threshold;
}

/**
 * Scroll to bottom only if the user is already near the bottom (smart auto-scroll).
 *
 * Dispatch pattern:
 * - autoScrollMode === "always" → force scroll (bypasses isChatAtBottom guard)
 * - isChatAtBottom() === true → user at bottom, scroll with new content
 * - else → show indicator, let user choose when to scroll
 */
export function smartScrollToBottom(): void {
	if (S.chatBatchLoading) return;
	ensureChatFollowTracking();
	if (S.autoScrollMode === "always") {
		scrollChatToBottom(true);
		return;
	}
	if (shouldFollowChat || isChatAtBottom()) {
		shouldFollowChat = true;
		scrollChatToBottom();
	} else {
		showNewContentIndicator();
	}
}

/// Incremented every time the chat view is emptied.
///
/// Work that renders detached nodes for the current view — an older history
/// page waiting on syntax highlighting — compares this before inserting them:
/// a session switch or `chat.clear` in the meantime means those nodes describe
/// a view that no longer exists. Streaming does not touch it, so a response in
/// progress never invalidates a page in flight.
let chatViewGeneration = 0;

export function chatViewEpoch(): number {
	return chatViewGeneration;
}

/// Empty the chat view and invalidate any detached render targeting it.
export function resetChatView(box: HTMLElement): void {
	unmountExecuteCommandToolBubbles(box);
	box.textContent = "";
	chatViewGeneration += 1;
}

/// Run `mutate`, which grows `box` above the viewport, keeping what the user
/// currently sees in place.
///
/// The resulting scroll correction is our own, not user intent: it is recorded
/// as programmatic so it cannot flip the chat out of follow mode.
export function preserveChatViewport(box: HTMLElement, mutate: () => void): void {
	const previousHeight = box.scrollHeight;
	const previousTop = box.scrollTop;
	mutate();
	const nextTop = previousTop + (box.scrollHeight - previousHeight);
	if (nextTop === box.scrollTop) return;
	box.scrollTop = nextTop;
	programmaticScrollTop = box.scrollTop;
}

/** Show the "new content" floating indicator on the chat area. */
export function showNewContentIndicator(): void {
	if (!S.chatMsgBox) return;
	let indicator = S.chatMsgBox.querySelector(".new-content-indicator") as HTMLButtonElement | null;
	if (!indicator) {
		indicator = document.createElement("button") as HTMLButtonElement;
		indicator.className = "new-content-indicator";
		indicator.type = "button";
		indicator.textContent = "↓ New messages";
		indicator.addEventListener("click", () => {
			scrollChatToBottom(true);
		});
		S.chatMsgBox.appendChild(indicator);
	}
}

/** Hide the "new content" floating indicator. */
export function hideNewContentIndicator(): void {
	if (!S.chatMsgBox) return;
	const indicator = S.chatMsgBox.querySelector(".new-content-indicator");
	if (indicator) indicator.remove();
}

export type MessageRole = "user" | "assistant" | "system" | "error";

export function chatAddMsg(cls: MessageRole, content: string, isHtml?: boolean): HTMLDivElement | null {
	const target = chatInsertionTarget();
	if (!target) return null;
	clearChatEmptyState();
	const el = document.createElement("div");
	el.className = `msg ${cls}`;
	if (cls === "system") {
		el.classList.add("system-notice");
	}
	if (isHtml) {
		// Safe: content is produced by renderMarkdown which escapes via esc() first,
		// then only adds our own formatting tags (pre, code, strong).
		el.innerHTML = content; // eslint-disable-line no-unsanitized/property
	} else {
		el.textContent = content;
	}
	target.appendChild(el);
	if (cls === "user") scrollChatToBottom(true);
	else smartScrollToBottom();
	return el;
}

/**
 * Add a user message with image thumbnails below the text.
 */
export function chatAddMsgWithImages(
	cls: MessageRole,
	htmlContent: string,
	images: ImageAttachment[],
): HTMLDivElement | null {
	return chatAddMsgWithAttachments(cls, htmlContent, images, []);
}

function appendHtmlContent(el: HTMLElement, htmlContent: string): void {
	if (!htmlContent) return;
	const textDiv = document.createElement("div");
	// Safe: htmlContent is produced by renderMarkdown which escapes user
	// input via esc() first, then only adds our own formatting tags.
	// This is the same pattern used in chatAddMsg above.
	textDiv.innerHTML = htmlContent; // eslint-disable-line no-unsanitized/property
	el.appendChild(textDiv);
}

function appendImageAttachments(el: HTMLElement, images: ImageAttachment[]): void {
	if (images.length === 0) return;
	const thumbRow = document.createElement("div");
	thumbRow.className = "msg-image-row";
	for (const img of images) {
		const thumb = document.createElement("img");
		thumb.className = "msg-image-thumb";
		thumb.src = img.dataUrl;
		thumb.alt = img.name;
		thumbRow.appendChild(thumb);
	}
	el.appendChild(thumbRow);
}

function appendDocumentAttachments(el: HTMLElement, documents: DocumentAttachment[]): void {
	for (const doc of documents) {
		const mediaSrc =
			doc.url ||
			(doc.stored_filename
				? `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(doc.stored_filename)}`
				: "#");
		renderDocument(el, mediaSrc, doc.display_name || doc.stored_filename, doc.mime_type, doc.size_bytes);
	}
}

export function chatAddMsgWithAttachments(
	cls: MessageRole,
	htmlContent: string,
	images: ImageAttachment[],
	documents: DocumentAttachment[],
): HTMLDivElement | null {
	const target = chatInsertionTarget();
	if (!target) return null;
	clearChatEmptyState();
	const el = document.createElement("div");
	el.className = `msg ${cls}`;
	appendHtmlContent(el, htmlContent);
	appendImageAttachments(el, images);
	appendDocumentAttachments(el, documents);
	target.appendChild(el);
	if (cls === "user") scrollChatToBottom(true);
	else smartScrollToBottom();
	return el;
}

export function stripChannelPrefix(text: string): string {
	return text.replace(/^\[Telegram(?:\s+from\s+[^\]]+)?\]\s*/, "");
}

export interface ChannelFooterInfo {
	channel_type?: string;
	username?: string;
	sender_name?: string;
	message_kind?: string;
}

export function appendChannelFooter(el: HTMLElement, channel: ChannelFooterInfo): void {
	const ft = document.createElement("div");
	ft.className = "msg-channel-footer";
	let label = channel.channel_type || "channel";
	const who = channel.username ? `@${channel.username}` : channel.sender_name;
	if (who) label += ` \u00b7 ${who}`;
	if (channel.message_kind === "voice") {
		const icon = document.createElement("span");
		icon.className = "voice-icon";
		icon.setAttribute("aria-hidden", "true");
		ft.appendChild(icon);
	}

	const text = document.createElement("span");
	text.textContent = `via ${label}`;
	ft.appendChild(text);
	el.appendChild(ft);
}

interface ReasoningDisclosureOptions {
	expanded?: boolean;
	streaming?: boolean;
}

function normalizedReasoningParts(reasoning: ReasoningContent | null | undefined): string[] {
	const parts = Array.isArray(reasoning) ? reasoning : [reasoning || ""];
	return parts.map((part) => part.trim()).filter(Boolean);
}

export function appendReasoningDisclosure(
	messageEl: HTMLElement | null,
	reasoning: ReasoningContent | null | undefined,
	options: ReasoningDisclosureOptions = {},
): HTMLDetailsElement | null {
	if (!messageEl) return null;
	const parts = normalizedReasoningParts(reasoning);
	const streaming = options.streaming === true;
	let details = messageEl.querySelector<HTMLDetailsElement>(".msg-reasoning");
	if (!(parts.length > 0 || streaming || details)) return null;
	if (!details) {
		details = document.createElement("details");
		details.className = "msg-reasoning";
		const summary = document.createElement("summary");
		summary.className = "msg-reasoning-summary";
		details.appendChild(summary);
		const body = document.createElement("div");
		body.className = "msg-reasoning-body";
		details.appendChild(body);
		if (messageEl.classList.contains("assistant")) messageEl.prepend(details);
		else messageEl.appendChild(details);
	}

	details.open = options.expanded ?? false;
	details.classList.toggle("is-streaming", streaming);
	const summary = details.querySelector<HTMLElement>(".msg-reasoning-summary");
	if (summary) summary.textContent = streaming ? "Thinking" : "Reasoning";
	const body = details.querySelector<HTMLElement>(".msg-reasoning-body");
	if (body) {
		body.dataset.reasoning = JSON.stringify(reasoning ?? "");
		body.hidden = parts.length === 0;
		syncReasoningParts(body, parts);
	}
	return details;
}

/// Update the rendered reasoning parts in place.
///
/// Streaming calls this on every chunk. Rebuilding the whole body each time
/// would replace nodes that did not change, which makes the bubble flicker and
/// keeps resetting the chat scroll position. Only parts whose text actually
/// changed are re-rendered, and only surplus parts are removed.
function syncReasoningParts(body: HTMLElement, parts: string[]): void {
	const rendered = Array.from(body.children).filter((child): child is HTMLElement => child instanceof HTMLElement);
	for (let index = parts.length; index < rendered.length; index += 1) {
		rendered[index].remove();
	}
	parts.forEach((part, index) => {
		const existing = rendered[index];
		if (existing) {
			if (existing.dataset.reasoningPart === part) return;
			existing.dataset.reasoningPart = part;
			existing.textContent = "";
			// Safe: renderMarkdown escapes source text before adding formatting tags.
			existing.insertAdjacentHTML("afterbegin", renderMarkdown(part));
			return;
		}
		const item = document.createElement("div");
		item.className = "msg-reasoning-item markdown-content";
		item.dataset.reasoningPart = part;
		// Safe: renderMarkdown escapes source text before adding formatting tags.
		item.insertAdjacentHTML("afterbegin", renderMarkdown(part));
		body.appendChild(item);
	});
}

export function chatAddErrorCard(err: ErrorCardData): void {
	if (!S.chatMsgBox) return;
	clearChatEmptyState();
	const el = document.createElement("div");
	el.className = "msg error-card";

	const icon = document.createElement("div");
	icon.className = "error-icon";
	icon.textContent = err.icon || "\u26A0\uFE0F";
	el.appendChild(icon);

	const body = document.createElement("div");
	body.className = "error-body";

	const title = document.createElement("div");
	title.className = "error-title";
	title.textContent = err.title;
	body.appendChild(title);

	if (err.detail) {
		const detail = document.createElement("div");
		detail.className = "error-detail";
		detail.textContent = err.detail;
		body.appendChild(detail);
	}

	if (err.provider) {
		const prov = document.createElement("div");
		prov.className = "error-detail";
		prov.textContent = `Provider: ${err.provider}`;
		prov.style.marginTop = "4px";
		prov.style.opacity = "0.6";
		body.appendChild(prov);
	}

	el.appendChild(body);

	S.chatMsgBox.appendChild(el);
	smartScrollToBottom();
}

export function chatAddErrorMsg(message: string): void {
	chatAddErrorCard(parseErrorMessage(message));
}

export function renderApprovalCard(requestId: string, command: string): void {
	if (!S.chatMsgBox) return;
	clearChatEmptyState();
	const card = S.cloneRequiredTemplateRoot<HTMLElement>("tpl-approval-card");
	card.id = `approval-${requestId}`;

	(card.querySelector(".approval-cmd") as HTMLElement).textContent = command;

	const allowBtn = card.querySelector(".approval-allow") as HTMLButtonElement;
	const denyBtn = card.querySelector(".approval-deny") as HTMLButtonElement;
	allowBtn.onclick = () => {
		resolveApproval(requestId, "approved", card);
	};
	denyBtn.onclick = () => {
		resolveApproval(requestId, "denied", card);
	};

	const countdown = card.querySelector(".approval-countdown") as HTMLElement;
	let remaining = 120;
	const timer = setInterval(() => {
		remaining--;
		countdown.textContent = `${remaining}s`;
		if (remaining <= 0) {
			clearInterval(timer);
			card.classList.add("approval-expired");
			allowBtn.disabled = true;
			denyBtn.disabled = true;
			countdown.textContent = "expired";
		}
	}, 1000);
	countdown.textContent = `${remaining}s`;

	S.chatMsgBox.appendChild(card);
	smartScrollToBottom();
}

export function resolveApproval(requestId: string, decision: string, card: HTMLElement): void {
	sendRpc("command.approval.resolve", { requestId, decision }).then(() => {
		card.classList.add("approval-resolved");
		card.querySelectorAll<HTMLButtonElement>(".approval-btn").forEach((b) => {
			b.disabled = true;
		});
		const status = document.createElement("div");
		status.className = "approval-status";
		status.textContent = decision === "approved" ? "Allowed" : "Denied";
		card.appendChild(status);
	});
}

export function highlightAndScroll(msgEls: (HTMLElement | null)[], messageIndex: number, query: string): void {
	let target: HTMLElement | null = null;
	if (messageIndex >= 0 && messageIndex < msgEls.length && msgEls[messageIndex]) {
		target = msgEls[messageIndex];
	}
	const lowerQ = query.toLowerCase();
	if (!target || (target.textContent || "").toLowerCase().indexOf(lowerQ) === -1) {
		for (const candidate of msgEls) {
			if (candidate && (candidate.textContent || "").toLowerCase().indexOf(lowerQ) !== -1) {
				target = candidate;
				break;
			}
		}
	}
	if (!target) return;
	msgEls.forEach((el) => {
		if (el) highlightTermInElement(el, query);
	});
	target.scrollIntoView({ behavior: "smooth", block: "center" });
	target.classList.add("search-highlight-msg");
	setTimeout(() => {
		if (!S.chatMsgBox) return;
		S.chatMsgBox.querySelectorAll("mark.search-term-highlight").forEach((m) => {
			const parent = m.parentNode;
			if (!parent) return;
			parent.replaceChild(document.createTextNode(m.textContent || ""), m);
			parent.normalize();
		});
		S.chatMsgBox.querySelectorAll(".search-highlight-msg").forEach((el) => {
			el.classList.remove("search-highlight-msg");
		});
	}, 5000);
}

export function highlightTermInElement(el: HTMLElement, query: string): void {
	const walker = document.createTreeWalker(el, NodeFilter.SHOW_TEXT, null);
	const nodes: Text[] = [];
	while (walker.nextNode()) nodes.push(walker.currentNode as Text);
	const lowerQ = query.toLowerCase();
	nodes.forEach((textNode) => {
		const text = textNode.nodeValue || "";
		const lowerText = text.toLowerCase();
		let idx = lowerText.indexOf(lowerQ);
		if (idx === -1) return;
		const frag = document.createDocumentFragment();
		let pos = 0;
		while (idx !== -1) {
			if (idx > pos) frag.appendChild(document.createTextNode(text.substring(pos, idx)));
			const mark = document.createElement("mark");
			mark.className = "search-term-highlight";
			mark.textContent = text.substring(idx, idx + query.length);
			frag.appendChild(mark);
			pos = idx + query.length;
			idx = lowerText.indexOf(lowerQ, pos);
		}
		if (pos < text.length) frag.appendChild(document.createTextNode(text.substring(pos)));
		textNode.parentNode?.replaceChild(frag, textNode);
	});
}

export function chatAutoResize(): void {
	if (!S.chatInput) return;
	S.chatInput.style.height = "auto";
	S.chatInput.style.height = `${Math.min(S.chatInput.scrollHeight, 120)}px`;
}

export function setComposerStopButton(active: boolean, sessionKey: string = S.activeSessionKey): void {
	const btn = S.$<HTMLButtonElement>("sendBtn");
	if (!btn) return;
	const icon = btn.querySelector(".icon");
	btn.classList.toggle("is-stop", active);
	btn.classList.remove("is-stopping");
	btn.dataset.mode = active ? "stop" : "send";
	btn.dataset.stopSessionKey = active ? sessionKey : "";
	btn.title = active ? "Stop generation" : "Send";
	btn.setAttribute("aria-label", active ? "Stop generation" : "Send");
	btn.disabled = active ? false : !S.connected;
	if (icon) {
		icon.classList.toggle("icon-arrow-up", !active);
		icon.classList.toggle("icon-stop", active);
	}
}

function contextBudgetPercent(contextBudget: ContextBudgetMetadata): number | null {
	const { promptTokens, compactionBudget } = contextBudget;
	if (
		!Number.isFinite(promptTokens) ||
		promptTokens < 0 ||
		!Number.isFinite(compactionBudget) ||
		compactionBudget <= 0
	) {
		return null;
	}
	return Math.floor((promptTokens * 100) / compactionBudget);
}

function tokenBarUsageText(total: number): string {
	let text = formatTokens(total);
	if (S.sessionContextWindow > 0) {
		const pct = Math.min(100, Math.max(0, Math.round((total / S.sessionContextWindow) * 100)));
		text += ` (${pct}%)`;
	}
	if (text === "0 (0%)") {
		text = "";
	}
	if (!S.sessionToolsEnabled) {
		text += `${text ? " \u00b7 " : ""}Tools: disabled`;
	}
	return text;
}

function tokenBarBudgetText(bar: HTMLElement, contextBudget?: ContextBudgetMetadata | null): string {
	if (contextBudget === null) return "";
	if (contextBudget === undefined) {
		return bar.querySelector<HTMLElement>("[data-context-budget-percent]")?.textContent || "";
	}
	const percent = contextBudgetPercent(contextBudget);
	return percent === null ? "" : `[${percent}%]`;
}

function appendTokenBarBudget(bar: HTMLElement, budgetText: string): void {
	if (!budgetText) return;
	const budgetEl = document.createElement("span");
	budgetEl.dataset.contextBudgetPercent = "true";
	budgetEl.textContent = budgetText;
	bar.append(" ", budgetEl);
}

export function updateTokenBar(contextBudget?: ContextBudgetMetadata | null): void {
	const bar = S.$("tokenBar");
	if (!bar) return;
	const budgetText = tokenBarBudgetText(bar, contextBudget);
	const total =
		S.sessionCurrentContextTokens || S.sessionCurrentInputTokens || S.sessionTokens.input + S.sessionTokens.output;
	bar.title = total > 0 ? "Context tokens used by the latest assistant turn" : "";
	bar.textContent = tokenBarUsageText(total);
	appendTokenBarBudget(bar, budgetText);
}
