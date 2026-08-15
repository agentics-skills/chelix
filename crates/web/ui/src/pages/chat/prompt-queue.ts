// ── Queued user prompts ──────────────────────────────────────
//
// The queue lives on the server: it is loaded with the session, updated by
// `chat` events, and rendered from that state. Nothing here is derived from
// local DOM, so a reload or a second client shows the same pending prompts.

import { renderMarkdown, sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { sessionStore } from "../../stores/session-store";
import type { QueuedPrompt } from "../../types/ws-events";

const queuesBySession = new Map<string, QueuedPrompt[]>();

/** Replace the known queue of a session and re-render when it is active. */
export function setQueuedPrompts(sessionKey: string, prompts: QueuedPrompt[]): void {
	if (prompts.length > 0) {
		queuesBySession.set(sessionKey, prompts);
	} else {
		queuesBySession.delete(sessionKey);
	}
	if (sessionKey === sessionStore.activeSessionKey.value) renderQueuedPrompts();
}

/** Queue of a session as last reported by the server. */
function getQueuedPrompts(sessionKey: string): QueuedPrompt[] {
	return queuesBySession.get(sessionKey) ?? [];
}

/** Render the queue of the active session into the composer tray. */
export function renderQueuedPrompts(): void {
	const tray = document.getElementById("queuedMessages");
	if (!tray) return;
	const prompts = getQueuedPrompts(sessionStore.activeSessionKey.value);
	tray.textContent = "";
	tray.classList.toggle("hidden", prompts.length === 0);
	for (const prompt of prompts) tray.appendChild(buildQueuedPromptElement(prompt));
}

function buildQueuedPromptElement(prompt: QueuedPrompt): HTMLElement {
	const el = document.createElement("div");
	el.className = "msg user queued";
	el.dataset.promptId = prompt.id;

	const body = document.createElement("span");
	// Safe: renderMarkdown escapes all input before applying formatting tags.
	body.insertAdjacentHTML("afterbegin", renderMarkdown(prompt.preview));
	el.appendChild(body);

	const badge = document.createElement("div");
	badge.className = "queued-badge";
	const label = document.createElement("span");
	label.className = "queued-label";
	label.textContent = t("chat:queued");
	const cancelBtn = document.createElement("button");
	cancelBtn.type = "button";
	cancelBtn.className = "queued-cancel";
	cancelBtn.title = t("chat:queuedMessages.cancelTooltip");
	cancelBtn.textContent = "\u2715";
	cancelBtn.addEventListener("click", (event: MouseEvent) => {
		event.stopPropagation();
		void cancelQueuedPrompt(prompt.sessionKey, prompt.id);
	});
	badge.appendChild(label);
	badge.appendChild(cancelBtn);
	el.appendChild(badge);
	return el;
}

/** Cancel one queued prompt. The server broadcasts the resulting queue. */
async function cancelQueuedPrompt(sessionKey: string, promptId: string): Promise<void> {
	await sendRpc("chat.prompt_queue.cancel", { sessionKey, promptId });
}
