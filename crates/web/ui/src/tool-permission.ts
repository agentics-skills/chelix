import { sendRpc } from "./helpers";
import * as S from "./state";
import { sessionStore } from "./stores/session-store";
import { renderToolCardResult, setToolCardStatus, type ToolResultRenderOptions } from "./tool-call-card";
import { isTerminalToolLifecycle } from "./tool-lifecycle";
import type { ToolLifecycleEvent } from "./types/ws-events";
import { showToast } from "./ui";

export type ToolPermissionMode = "auto" | "moderated";
export type ToolPermissionType = "manual";
export type ToolPermissionPhase = "before_execution" | "after_result";

type ToolScreenshotMode = ToolResultRenderOptions["screenshotMode"];

interface StoredToolLifecycle {
	lifecycle: ToolLifecycleEvent;
	screenshotMode?: ToolScreenshotMode;
}

export interface PendingToolPermission {
	sessionKey: string;
	runId: string;
	toolCallId: string;
	toolName: string;
	phase: ToolPermissionPhase;
}

const pendingByTool = new Map<string, PendingToolPermission>();
const lastLifecycleByTool = new Map<string, StoredToolLifecycle>();
let denyModal: HTMLElement | null = null;
let denyFeedback: HTMLTextAreaElement | null = null;
let denySend: HTMLButtonElement | null = null;
let denyTarget: PendingToolPermission | null = null;

function pendingKey(item: Pick<PendingToolPermission, "sessionKey" | "toolCallId" | "phase">): string {
	return `${item.sessionKey}:${item.toolCallId}:${item.phase}`;
}

function lifecycleStoreKey(sessionKey: string, toolCallId: string): string {
	return JSON.stringify([sessionKey, toolCallId]);
}

export function rememberToolLifecycle(
	sessionKey: string,
	lifecycle: ToolLifecycleEvent,
	screenshotMode?: ToolScreenshotMode,
): void {
	const key = lifecycleStoreKey(sessionKey, lifecycle.toolCallId);
	if (isTerminalToolLifecycle(lifecycle)) {
		lastLifecycleByTool.delete(key);
		return;
	}
	lastLifecycleByTool.set(key, { lifecycle, screenshotMode });
}

function forgetSessionLifecycles(sessionKey: string): void {
	for (const key of [...lastLifecycleByTool.keys()]) {
		const parsed: unknown = JSON.parse(key);
		if (Array.isArray(parsed) && parsed[0] === sessionKey) lastLifecycleByTool.delete(key);
	}
}

function currentSessionPermission(): { mode: ToolPermissionMode; type: ToolPermissionType } {
	const session = sessionStore.getByKey(S.activeSessionKey);
	const mode = session?.toolPermissionMode === "moderated" ? "moderated" : "auto";
	return { mode, type: "manual" };
}

function findToolCard(toolCallId: string): HTMLElement | null {
	if (!S.chatMsgBox) return null;
	return S.chatMsgBox.querySelector(`.tool-call-card[data-tool-call-id="${CSS.escape(toolCallId)}"]`);
}

function clearActions(card: HTMLElement): void {
	const actions = card.querySelector(".tool-call-permission-actions");
	if (actions) actions.replaceChildren();
}

function renderResultIfNeeded(
	card: HTMLElement,
	lifecycle: ToolLifecycleEvent,
	screenshotMode?: ToolScreenshotMode,
): void {
	if (lifecycle.stage !== "result_ready" || lifecycle.result == null) return;
	let parsed: unknown = lifecycle.result;
	try {
		parsed = JSON.parse(lifecycle.result) as unknown;
	} catch {
		parsed = lifecycle.result;
	}
	renderToolCardResult(card, parsed as never, {
		sessionKey: S.activeSessionKey || "main",
		screenshotMode,
	});
}

export function applyToolPermissionOverlay(
	card: HTMLElement,
	lifecycle?: ToolLifecycleEvent,
	screenshotMode?: ToolScreenshotMode,
): void {
	const toolCallId = card.dataset.toolCallId;
	if (!toolCallId) return;
	const pending = [...pendingByTool.values()].find(
		(item) => item.sessionKey === S.activeSessionKey && item.toolCallId === toolCallId,
	);
	if (!pending) {
		clearActions(card);
		return;
	}
	if (lifecycle && isTerminalToolLifecycle(lifecycle)) {
		clearActions(card);
		return;
	}
	setToolCardStatus(card, "running", "manual");
	if (lifecycle) renderResultIfNeeded(card, lifecycle, screenshotMode);
	const actions = card.querySelector(".tool-call-permission-actions");
	if (!actions) return;
	if (actions.childElementCount > 0) return;
	for (const decision of ["approve", "deny", "skip"] as const) {
		const button = document.createElement("button");
		button.type = "button";
		button.className = `tool-call-permission-btn tool-call-permission-${decision}`;
		button.textContent = decision;
		button.addEventListener("click", (event) => {
			event.preventDefault();
			event.stopPropagation();
			if (decision === "deny") {
				openDenyModal(pending);
				return;
			}
			void resolvePermission(pending, decision).catch((error: unknown) => {
				showToast(error instanceof Error ? error.message : "Permission resolve failed", "error");
			});
		});
		actions.appendChild(button);
	}
}

async function resolvePermission(
	pending: PendingToolPermission,
	decision: "approve" | "skip" | "deny",
	feedback?: string,
): Promise<void> {
	const payload: Record<string, string> = {
		sessionKey: pending.sessionKey,
		toolCallId: pending.toolCallId,
		phase: pending.phase,
		decision,
	};
	if (decision === "deny") payload.feedback = feedback || "";
	const response = await sendRpc("tool.permission.resolve", payload);
	if (!response.ok) throw new Error(response.error?.message || "Permission resolve failed");
}

function ensureDenyModal(): void {
	if (denyModal) return;
	const backdrop = document.createElement("div");
	backdrop.id = "toolPermissionDenyModal";
	backdrop.className = "provider-modal-backdrop hidden";
	const modal = document.createElement("div");
	modal.className = "provider-modal";
	const header = document.createElement("div");
	header.className = "provider-modal-header";
	const title = document.createElement("div");
	title.className = "provider-item-name";
	title.textContent = "Deny tool call";
	header.appendChild(title);
	modal.appendChild(header);
	const body = document.createElement("div");
	body.className = "provider-modal-body flex flex-col gap-2";
	const textarea = document.createElement("textarea");
	textarea.className = "w-full min-h-24 text-sm";
	textarea.setAttribute("aria-label", "Feedback");
	body.appendChild(textarea);
	const buttons = document.createElement("div");
	buttons.className = "flex justify-end gap-2";
	const cancel = document.createElement("button");
	cancel.type = "button";
	cancel.className = "provider-btn provider-btn-secondary";
	cancel.textContent = "CANCEL";
	const send = document.createElement("button");
	send.type = "button";
	send.className = "provider-btn";
	send.textContent = "SEND";
	send.disabled = true;
	buttons.append(cancel, send);
	body.appendChild(buttons);
	modal.appendChild(body);
	backdrop.appendChild(modal);
	document.body.appendChild(backdrop);
	denyModal = backdrop;
	denyFeedback = textarea;
	denySend = send;
	textarea.addEventListener("input", () => {
		send.disabled = textarea.value.trim().length === 0;
	});
	cancel.addEventListener("click", closeDenyModal);
	backdrop.addEventListener("click", (event) => {
		if (event.target === backdrop) closeDenyModal();
	});
	send.addEventListener("click", () => {
		const target = denyTarget;
		const text = textarea.value.trim();
		if (!(target && text)) return;
		void resolvePermission(target, "deny", text)
			.then(closeDenyModal)
			.catch((error: unknown) => {
				showToast(error instanceof Error ? error.message : "Permission resolve failed", "error");
			});
	});
}

function openDenyModal(pending: PendingToolPermission): void {
	ensureDenyModal();
	denyTarget = pending;
	if (denyFeedback) denyFeedback.value = "";
	if (denySend) denySend.disabled = true;
	denyModal?.classList.remove("hidden");
	denyFeedback?.focus();
}

function closeDenyModal(): void {
	denyModal?.classList.add("hidden");
	denyTarget = null;
}

export function rememberPendingToolPermissions(items: PendingToolPermission[] | undefined): void {
	const active = S.activeSessionKey;
	for (const key of [...pendingByTool.keys()]) {
		const item = pendingByTool.get(key);
		if (item?.sessionKey === active) pendingByTool.delete(key);
	}
	if (active) forgetSessionLifecycles(active);
	for (const item of items || []) pendingByTool.set(pendingKey(item), item);
	refreshVisiblePermissionCards();
}

export function handleToolPermissionRequested(payload: Record<string, unknown>): void {
	if (typeof payload.sessionKey !== "string" || typeof payload.toolCallId !== "string") return;
	if (payload.phase !== "before_execution" && payload.phase !== "after_result") return;
	const item: PendingToolPermission = {
		sessionKey: payload.sessionKey,
		runId: typeof payload.runId === "string" ? payload.runId : "",
		toolCallId: payload.toolCallId,
		toolName: typeof payload.toolName === "string" ? payload.toolName : "",
		phase: payload.phase,
	};
	pendingByTool.set(pendingKey(item), item);
	const card = findToolCard(item.toolCallId);
	if (card && item.sessionKey === S.activeSessionKey) {
		const stored = lastLifecycleByTool.get(lifecycleStoreKey(item.sessionKey, item.toolCallId));
		applyToolPermissionOverlay(card, stored?.lifecycle, stored?.screenshotMode);
	}
}

export function handleToolPermissionResolved(payload: Record<string, unknown>): void {
	if (typeof payload.sessionKey !== "string" || typeof payload.toolCallId !== "string") return;
	if (payload.phase !== "before_execution" && payload.phase !== "after_result") return;
	pendingByTool.delete(
		pendingKey({
			sessionKey: payload.sessionKey,
			toolCallId: payload.toolCallId,
			phase: payload.phase,
		}),
	);
	const card = findToolCard(payload.toolCallId);
	if (card) clearActions(card);
}

function refreshVisiblePermissionCards(): void {
	if (!S.chatMsgBox) return;
	for (const card of S.chatMsgBox.querySelectorAll<HTMLElement>(".tool-call-card")) {
		applyToolPermissionOverlay(card);
	}
}

async function patchPermission(fields: Record<string, string>): Promise<void> {
	const key = S.activeSessionKey;
	if (!key) return;
	const response = await sendRpc("sessions.patch", { key, ...fields });
	if (!response.ok) throw new Error(response.error?.message || "Failed to update tool permission");
}

export function syncToolPermissionToolbar(): void {
	const modeBtn = S.$("toolPermissionModeBtn");
	const typeWrap = S.$("toolPermissionTypeWrap");
	const typeSelect = S.$("toolPermissionTypeSelect") as HTMLSelectElement | null;
	if (!modeBtn) return;
	const { mode } = currentSessionPermission();
	modeBtn.textContent = mode;
	modeBtn.setAttribute("data-mode", mode);
	modeBtn.setAttribute("title", `Tool permission mode: ${mode}`);
	typeWrap?.classList.toggle("hidden", mode !== "moderated");
	if (typeSelect) typeSelect.value = "manual";
}

export function mountToolPermissionToolbar(): void {
	const modeBtn = S.$("toolPermissionModeBtn");
	modeBtn?.addEventListener("click", () => {
		const next = currentSessionPermission().mode === "auto" ? "moderated" : "auto";
		void patchPermission({ toolPermissionMode: next })
			.then(syncToolPermissionToolbar)
			.catch((error: unknown) => {
				showToast(error instanceof Error ? error.message : "Failed to update tool permission", "error");
			});
	});
	const typeSelect = S.$("toolPermissionTypeSelect") as HTMLSelectElement | null;
	typeSelect?.addEventListener("change", () => {
		void patchPermission({ toolPermissionType: "manual" })
			.then(syncToolPermissionToolbar)
			.catch((error: unknown) => {
				showToast(error instanceof Error ? error.message : "Failed to update tool permission", "error");
			});
	});
	syncToolPermissionToolbar();
}
