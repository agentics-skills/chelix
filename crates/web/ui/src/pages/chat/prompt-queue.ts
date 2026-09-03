// ── Queued user prompts ──────────────────────────────────────

import { chatAddMsg } from "../../chat-ui";
import { renderMarkdown, sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { sessionStore } from "../../stores/session-store";
import type { QueuedPrompt, QueuedPromptContent, QueuedPromptsStatus } from "../../types/ws-events";

function renderPromptList(prompts: QueuedPrompt[]): void {
	const tray = document.getElementById("queuedMessages");
	if (!tray) return;
	tray.textContent = "";
	tray.classList.toggle("hidden", prompts.length === 0);
	for (const prompt of prompts) tray.appendChild(buildQueuedPromptElement(prompt));
}

/** Replace the active session dock with one complete backend status. */
export function replaceQueuedPromptsDock(status: QueuedPromptsStatus): void {
	if (status.sessionKey !== sessionStore.activeSessionKey.value) return;
	renderPromptList(status.prompts);
}

/** Clear the dock while a different session is loading. */
export function clearQueuedPromptsDock(): void {
	renderPromptList([]);
}

function queuedPromptPreview(content: QueuedPromptContent): string {
	const messageText =
		typeof content.content === "string"
			? content.content
			: content.content
					.filter((block) => block.type === "text")
					.map((block) => block.text)
					.join("\n");
	if (messageText.trim()) return messageText;
	return (content.documents ?? []).map((document) => document.displayName).join(", ");
}

function buildQueuedPromptElement(prompt: QueuedPrompt): HTMLElement {
	const el = document.createElement("div");
	el.className = "msg user queued";
	el.dataset.promptId = String(prompt.id);

	const body = document.createElement("span");
	// Safe: renderMarkdown escapes all input before applying formatting tags.
	body.insertAdjacentHTML("afterbegin", renderMarkdown(queuedPromptPreview(prompt.content)));
	el.appendChild(body);

	const badge = document.createElement("div");
	badge.className = "queued-badge";
	const label = document.createElement("span");
	label.className = "queued-label";
	label.textContent = t("chat:queued");
	const removeButton = document.createElement("button");
	removeButton.type = "button";
	removeButton.className = "queued-cancel";
	removeButton.title = t("chat:queuedMessages.cancelTooltip");
	removeButton.textContent = "\u2715";
	removeButton.addEventListener("click", (event: MouseEvent) => {
		event.stopPropagation();
		void removeQueuedPrompt(prompt.id);
	});
	badge.appendChild(label);
	badge.appendChild(removeButton);
	el.appendChild(badge);
	return el;
}

async function removeQueuedPrompt(id: number): Promise<void> {
	try {
		const response = await sendRpc("chat.queued_prompts.remove", { id });
		if (response.ok && response.payload) {
			replaceQueuedPromptsDock(response.payload);
			return;
		}
		chatAddMsg("error", response.error?.message || "Failed to remove queued prompt");
	} catch (error) {
		chatAddMsg("error", error instanceof Error ? error.message : "Failed to remove queued prompt");
	}
}
