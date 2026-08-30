// ── Model selector ──────────────────────────────────────────

import { sendRpc } from "./helpers";
import { t } from "./i18n";
import { showModelNotice } from "./pages/ChatPage";
import * as S from "./state";
import { modelStore } from "./stores/model-store";
import { sessionStore } from "./stores/session-store";
import type { ModelInfo } from "./types/model";
import type { RpcResponse } from "./types/rpc";
import type { SessionModelSelection, SessionPatchPayload } from "./types/session";
import { showToast } from "./ui";

type ConfirmedSessionModelPayload = SessionPatchPayload & SessionModelSelection;

function isConfirmedSessionModelPayload(
	payload: unknown,
	sessionKey: string,
): payload is ConfirmedSessionModelPayload {
	if (!payload || typeof payload !== "object") return false;
	const value = payload as Partial<SessionPatchPayload>;
	return (
		value.key === sessionKey &&
		typeof value.model === "string" &&
		value.model.length > 0 &&
		typeof value.reasoningEffort === "string" &&
		value.reasoningEffort.length > 0 &&
		Number.isInteger(value.version) &&
		(value.version as number) >= 0
	);
}

function applyConfirmedSessionModel(payload: ConfirmedSessionModelPayload): void {
	const session = sessionStore.getByKey(payload.key);
	if (session) {
		if (payload.version < session.version) {
			restoreConfirmedSessionModel(payload.key);
			return;
		}
		session.model = payload.model;
		session.reasoningEffort = payload.reasoningEffort;
		session.version = payload.version;
		session.dataVersion.value++;
	}
	if (sessionStore.activeSessionKey.value !== payload.key) return;
	modelStore.select(payload.model);
	modelStore.setReasoningEffort(payload.reasoningEffort);
	localStorage.setItem("chelix-model", payload.model);
	const model = modelStore.getById(payload.model);
	if (model) updateModelComboLabel(model);
}

export function requireSessionModelState(sessionKey: string): boolean {
	if (sessionStore.getByKey(sessionKey)) return true;
	showToast(t("chat:sessionStateUnavailable"), "error");
	return false;
}

function restoreConfirmedSessionModel(sessionKey: string): void {
	const session = sessionStore.getByKey(sessionKey);
	if (!(session && sessionStore.activeSessionKey.value === sessionKey)) return;
	modelStore.select(session.model);
	modelStore.setReasoningEffort(session.reasoningEffort);
	if (session.model) {
		localStorage.setItem("chelix-model", session.model);
	} else {
		localStorage.removeItem("chelix-model");
	}
	const model = modelStore.getById(session.model);
	if (model) {
		updateModelComboLabel(model);
	} else if (S.modelComboLabel) {
		S.modelComboLabel.textContent = session.model;
		S.modelComboLabel.title = session.model;
	}
}

export async function setSessionModel(
	sessionKey: string,
	selection: SessionModelSelection,
): Promise<RpcResponse<SessionPatchPayload>> {
	if (!requireSessionModelState(sessionKey)) {
		return {
			ok: false,
			error: { code: "UNAVAILABLE", message: t("chat:sessionStateUnavailable") },
		};
	}
	try {
		const response = await sendRpc("sessions.patch", { key: sessionKey, ...selection });
		if (!response.ok) {
			restoreConfirmedSessionModel(sessionKey);
			showToast(response.error?.message || "Failed to update session model", "error");
			return response;
		}
		if (!isConfirmedSessionModelPayload(response.payload, sessionKey)) {
			restoreConfirmedSessionModel(sessionKey);
			const invalidResponse: RpcResponse<SessionPatchPayload> = {
				ok: false,
				error: {
					code: "INVALID_RESPONSE",
					message: "Session model update returned invalid state",
				},
			};
			showToast(invalidResponse.error?.message || "Failed to update session model", "error");
			return invalidResponse;
		}
		applyConfirmedSessionModel(response.payload);
		return response;
	} catch (error) {
		restoreConfirmedSessionModel(sessionKey);
		const message = error instanceof Error ? error.message : "Failed to update session model";
		showToast(message, "error");
		return { ok: false, error: { code: "UNAVAILABLE", message } };
	}
}

function modelSelection(model: ModelInfo): SessionModelSelection {
	return {
		model: model.id,
		reasoningEffort: modelStore.reasoningEffortForModel(model),
	};
}

export function selectedModelSelection(): SessionModelSelection | null {
	const model = modelStore.selectedModel.value;
	return model ? modelSelection(model) : null;
}

export interface ModelLabelInfo {
	id: string;
}

export function modelDisplayLabel(model: ModelLabelInfo): string {
	return model.id;
}

export function modelTitle(model: ModelLabelInfo): string {
	return model.id;
}

function updateModelComboLabel(model: ModelInfo): void {
	if (!S.modelComboLabel) return;
	const label = modelDisplayLabel(model);
	S.modelComboLabel.textContent = label;
	S.modelComboLabel.title = modelTitle(model);
}

export function fetchModels(): Promise<void> {
	return modelStore.fetch().then(() => {
		const model = modelStore.selectedModel.value;
		if (model) updateModelComboLabel(model);

		if (S.modelDropdown && !S.modelDropdown.classList.contains("hidden")) {
			const query = S.modelSearchInput ? (S.modelSearchInput as HTMLInputElement).value.trim() : "";
			renderModelList(query);
		}
	});
}

export function selectModel(m: ModelInfo): void {
	const sessionKey = S.activeSessionKey;
	if (!requireSessionModelState(sessionKey)) return;
	const selection = modelSelection(m);
	modelStore.select(m.id);
	modelStore.setReasoningEffort(selection.reasoningEffort);
	updateModelComboLabel(m);
	void setSessionModel(sessionKey, selection).then((response) => {
		if (!(response.ok && response.payload)) return;
		const payload = response.payload;
		const session = sessionStore.getByKey(sessionKey);
		if (
			sessionStore.activeSessionKey.value === sessionKey &&
			payload.model === selection.model &&
			payload.reasoningEffort === selection.reasoningEffort &&
			session?.model === payload.model &&
			session.reasoningEffort === payload.reasoningEffort &&
			session.version === payload.version
		) {
			showModelNotice(m);
		}
	});
	closeModelDropdown();
}

export function openModelDropdown(): void {
	if (!S.modelDropdown) return;
	S.modelDropdown.classList.remove("hidden");
	(S.modelSearchInput as HTMLInputElement).value = "";
	S.setModelIdx(-1);
	renderModelList("");
	requestAnimationFrame(() => {
		if (S.modelSearchInput) S.modelSearchInput.focus();
	});
}

export function closeModelDropdown(): void {
	if (!S.modelDropdown) return;
	S.modelDropdown.classList.add("hidden");
	if (S.modelSearchInput) (S.modelSearchInput as HTMLInputElement).value = "";
	S.setModelIdx(-1);
}

function buildModelItem(m: ModelInfo, currentId: string): HTMLDivElement {
	const el = document.createElement("div");
	el.className = "model-dropdown-item";
	if (m.id === currentId) el.classList.add("selected");

	const label = document.createElement("span");
	label.className = "model-item-label";
	label.textContent = modelDisplayLabel(m);
	label.title = modelTitle(m);
	el.title = label.title;
	el.appendChild(label);

	const meta = document.createElement("span");
	meta.className = "model-item-meta";

	if (m.provider) {
		const prov = document.createElement("span");
		prov.className = "model-item-provider";
		prov.textContent = m.provider;
		meta.appendChild(prov);
	}

	const brainIcon = document.createElement("span");
	brainIcon.className = "icon icon-xs icon-brain";
	brainIcon.title = "Reasoning";
	brainIcon.style.cssText = "opacity:0.5;flex-shrink:0;";
	meta.appendChild(brainIcon);

	if (meta.childNodes.length > 0) el.appendChild(meta);
	el.addEventListener("click", () => selectModel(m));
	return el;
}

export function renderModelList(query: string): void {
	if (!S.modelDropdownList) return;
	S.modelDropdownList.textContent = "";
	const q = query.toLowerCase();
	const allModels = modelStore.models.value;
	const filtered = allModels.filter((m) => {
		const id = m.id.toLowerCase();
		const provider = (m.provider || "").toLowerCase();
		return !q || id.includes(q) || provider.includes(q);
	});
	if (filtered.length === 0) {
		const empty = document.createElement("div");
		empty.className = "model-dropdown-empty";
		empty.textContent = t("common:labels.noMatchingModels");
		S.modelDropdownList.appendChild(empty);
		return;
	}
	const currentId = modelStore.selectedModelId.value;
	let lastPreferredIdx = -1;
	for (let i = filtered.length - 1; i >= 0; i--) {
		if (filtered[i].preferred) {
			lastPreferredIdx = i;
			break;
		}
	}
	filtered.forEach((m, idx) => {
		S.modelDropdownList?.appendChild(buildModelItem(m, currentId));

		if (idx === lastPreferredIdx && lastPreferredIdx < filtered.length - 1) {
			const divider = document.createElement("div");
			divider.className = "model-dropdown-divider";
			S.modelDropdownList?.appendChild(divider);
		}
	});
}

function updateModelActive(): void {
	if (!S.modelDropdownList) return;
	const items = S.modelDropdownList.querySelectorAll<HTMLElement>(".model-dropdown-item");
	items.forEach((el, i) => {
		el.classList.toggle("kb-active", i === S.modelIdx);
	});
	if (S.modelIdx >= 0 && items[S.modelIdx]) {
		items[S.modelIdx].scrollIntoView({ block: "nearest" });
	}
}

function moveModelSelection(delta: number, itemCount: number): void {
	const nextIndex = delta > 0 ? Math.min(S.modelIdx + delta, itemCount - 1) : Math.max(S.modelIdx + delta, 0);
	S.setModelIdx(nextIndex);
	updateModelActive();
}

function selectModelFromKeyboard(items: NodeListOf<HTMLElement>): void {
	if (S.modelIdx >= 0 && items[S.modelIdx]) {
		items[S.modelIdx].click();
		return;
	}
	if (items.length === 1) items[0].click();
}

const modelSearchKeyHandlers: Record<string, (items: NodeListOf<HTMLElement>) => void> = {
	ArrowDown: (items) => moveModelSelection(1, items.length),
	ArrowUp: (items) => moveModelSelection(-1, items.length),
	Enter: selectModelFromKeyboard,
	Escape: () => {
		closeModelDropdown();
		S.modelComboBtn?.focus();
	},
};

function handleModelSearchKeydown(event: Event): void {
	const keyboardEvent = event as KeyboardEvent;
	const handler = modelSearchKeyHandlers[keyboardEvent.key];
	if (!handler) return;
	const items = S.modelDropdownList?.querySelectorAll<HTMLElement>(".model-dropdown-item");
	if (!items) return;
	keyboardEvent.preventDefault();
	handler(items);
}

export function bindModelComboEvents(): void {
	if (!(S.modelComboBtn && S.modelSearchInput && S.modelDropdownList && S.modelCombo)) return;

	S.modelComboBtn.addEventListener("click", () => {
		if (S.modelDropdown?.classList.contains("hidden")) {
			openModelDropdown();
		} else {
			closeModelDropdown();
		}
	});

	S.modelSearchInput.addEventListener("input", () => {
		S.setModelIdx(-1);
		renderModelList((S.modelSearchInput as HTMLInputElement).value.trim());
	});

	S.modelSearchInput.addEventListener("keydown", handleModelSearchKeydown);
}

document.addEventListener("click", (e: MouseEvent) => {
	if (S.modelCombo && !S.modelCombo.contains(e.target as Node)) {
		closeModelDropdown();
	}
});

window.addEventListener("chelix:locale-changed", () => {
	if (S.modelDropdown && !S.modelDropdown.classList.contains("hidden")) {
		const query = S.modelSearchInput ? (S.modelSearchInput as HTMLInputElement).value.trim() : "";
		renderModelList(query);
	}
});
