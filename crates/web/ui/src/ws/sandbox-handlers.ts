// ── Sandbox event handlers ───────────────────────────────────

import { chatAddMsg, smartScrollToBottom } from "../chat-ui";
import { currentPrefix } from "../router";
import * as S from "../state";
import type { SandboxPhasePayload } from "../types/ws-events";
import { clearChatEmptyState } from "./shared";

function updateSandboxBuildingFlag(building: boolean): void {
	const info = S.sandboxInfo;
	if (info) S.setSandboxInfo({ ...info, image_building: building });
}

let sandboxPrepareIndicatorEl: HTMLElement | null = null;
export function handleSandboxPrepare(payload: SandboxPhasePayload): void {
	const isChatPage = currentPrefix === "/chats";
	if (!isChatPage) return;

	if (payload.phase === "start") {
		if (sandboxPrepareIndicatorEl) {
			sandboxPrepareIndicatorEl.remove();
			sandboxPrepareIndicatorEl = null;
		}
		sandboxPrepareIndicatorEl = chatAddMsg(
			"system",
			"Preparing sandbox environment (first run may take a minute)\u2026",
		);
		return;
	}

	if (sandboxPrepareIndicatorEl) {
		sandboxPrepareIndicatorEl.remove();
		sandboxPrepareIndicatorEl = null;
	}

	if (payload.phase === "error") {
		chatAddMsg("error", `Sandbox setup failed: ${payload.error || "unknown"}`);
	}
}

let buildIndicatorEl: HTMLElement | null = null;
let buildTimerInterval: ReturnType<typeof setInterval> | null = null;
let buildStartTime = 0;

function clearBuildIndicator(): void {
	if (buildTimerInterval) {
		clearInterval(buildTimerInterval);
		buildTimerInterval = null;
	}
	if (buildIndicatorEl) {
		buildIndicatorEl.remove();
		buildIndicatorEl = null;
	}
}

function formatElapsed(ms: number): string {
	const secs = Math.floor(ms / 1000);
	const m = Math.floor(secs / 60);
	const s = secs % 60;
	return m > 0 ? `${m}m ${s}s` : `${s}s`;
}

export function handleSandboxImageBuild(payload: SandboxPhasePayload): void {
	const phase = payload.phase;
	// Update the sandboxInfo signal so all pages (chat, settings) reflect the build state.
	updateSandboxBuildingFlag(phase === "start");

	const isChatPage = currentPrefix === "/chats";
	if (!isChatPage) return;

	if (phase === "start") {
		clearBuildIndicator();
		buildStartTime = Date.now();

		buildIndicatorEl = document.createElement("div");
		buildIndicatorEl.className = "msg system download-indicator";

		const status = document.createElement("div");
		status.className = "download-status";
		const pkgCount = payload.package_count || 0;
		const pkgLabel = pkgCount > 0 ? ` (${pkgCount} packages)` : "";
		status.textContent = `Building sandbox image${pkgLabel}\u2026`;
		buildIndicatorEl.appendChild(status);

		const progressContainer = document.createElement("div");
		progressContainer.className = "download-progress indeterminate";
		const progressBar = document.createElement("div");
		progressBar.className = "download-progress-bar";
		progressContainer.appendChild(progressBar);
		buildIndicatorEl.appendChild(progressContainer);

		const progressText = document.createElement("div");
		progressText.className = "download-progress-text";
		progressText.textContent = "First run — usually takes 3\u20135 minutes";
		buildIndicatorEl.appendChild(progressText);

		if (S.chatMsgBox) {
			clearChatEmptyState();
			S.chatMsgBox.appendChild(buildIndicatorEl);
			smartScrollToBottom();
		}

		// Update elapsed time every second.
		buildTimerInterval = setInterval(() => {
			const textEl = buildIndicatorEl?.querySelector(".download-progress-text");
			if (textEl) {
				textEl.textContent = `Elapsed: ${formatElapsed(Date.now() - buildStartTime)}`;
			}
		}, 1000);
	} else if (phase === "done") {
		clearBuildIndicator();
		if (!payload.built) {
			// Image was already cached — no need to tell the user about it.
			return;
		}
		chatAddMsg("system", "Sandbox image ready");
	} else if (phase === "error") {
		clearBuildIndicator();
		chatAddMsg("error", `Sandbox image build failed: ${payload.error || "unknown"}`);
	}
}

export function handleSandboxImageProvision(payload: SandboxPhasePayload): void {
	const isChatPage = currentPrefix === "/chats";
	if (!isChatPage) return;
	if (payload.phase === "start") {
		chatAddMsg("system", "Provisioning sandbox packages\u2026");
	} else if (payload.phase === "done") {
		if (S.chatMsgBox?.lastChild) S.chatMsgBox.removeChild(S.chatMsgBox.lastChild);
		chatAddMsg("system", "Sandbox packages provisioned");
	} else if (payload.phase === "error") {
		if (S.chatMsgBox?.lastChild) S.chatMsgBox.removeChild(S.chatMsgBox.lastChild);
		chatAddMsg("error", `Sandbox provisioning failed: ${payload.error || "unknown"}`);
	}
}

export function handleBrowserImagePull(payload: SandboxPhasePayload): void {
	const isChatPage = currentPrefix === "/chats";
	if (!isChatPage) return;
	const image = payload.image || "browser container";
	if (payload.phase === "start") {
		chatAddMsg("system", `Pulling browser container image (${image})\u2026 This may take a few minutes on first run.`);
	} else if (payload.phase === "done") {
		if (S.chatMsgBox?.lastChild) S.chatMsgBox.removeChild(S.chatMsgBox.lastChild);
		chatAddMsg("system", `Browser container image ready: ${image}`);
	} else if (payload.phase === "error") {
		if (S.chatMsgBox?.lastChild) S.chatMsgBox.removeChild(S.chatMsgBox.lastChild);
		chatAddMsg("error", `Browser container image pull failed: ${payload.error || "unknown"}`);
	}
}
