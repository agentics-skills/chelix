// ── Model store (signal-based) ──────────────────────────────
//
// Single source of truth for model data. Both Preact components
// (auto-subscribe) and imperative code (read .value) can use this.

import { computed, signal } from "@preact/signals";
import { sendRpc } from "../helpers";
import type { ModelInfo } from "../types/model";
import type { RpcResponse } from "../types/rpc";

// ── Signals ──────────────────────────────────────────────────
export const models = signal<ModelInfo[]>([]);
export const selectedModelId = signal<string>(localStorage.getItem("chelix-model") || "");
export const reasoningEffort = signal<string | null>(localStorage.getItem("chelix-reasoning-effort"));

export const selectedModel = computed<ModelInfo | null>(() => {
	const id = selectedModelId.value;
	return models.value.find((m) => m.id === id) || null;
});

/** Reasoning efforts supported by the currently selected model. */
export const supportedReasoningEfforts = computed<string[]>(() => {
	return selectedModel.value?.reasoning_supported_efforts || [];
});

// ── Methods ──────────────────────────────────────────────────

/** Replace the full model list (e.g. after fetch or bootstrap). */
export function setAll(arr: ModelInfo[]): void {
	models.value = arr || [];
}

/** Return the selected compatible effort or the model's first configured effort. */
export function reasoningEffortForModel(model: ModelInfo): string {
	const selectedEffort = reasoningEffort.value;
	return selectedEffort !== null && model.reasoning_supported_efforts.includes(selectedEffort)
		? selectedEffort
		: model.reasoning_supported_efforts[0];
}

/** Fetch models from the server via RPC. */
export function fetch(): Promise<void> {
	return sendRpc("models.list", {}).then((r) => {
		const res = r as RpcResponse<ModelInfo[]>;
		if (!res?.ok) return;
		setAll(res.payload || []);
		if (models.value.length === 0) return;
		const saved = localStorage.getItem("chelix-model") || "";
		const found = models.value.find((m) => m.id === saved);
		const model = found || models.value[0];
		select(model.id);
		setReasoningEffort(reasoningEffortForModel(model));
		if (!found) localStorage.setItem("chelix-model", model.id);
	});
}

/** Select a model by id. Persists to localStorage. */
export function select(id: string): void {
	selectedModelId.value = id;
}

/** Set an exact reasoning effort or clear unavailable session state. */
export function setReasoningEffort(effort: string | null): void {
	reasoningEffort.value = effort;
	if (effort === null) {
		localStorage.removeItem("chelix-reasoning-effort");
	} else {
		localStorage.setItem("chelix-reasoning-effort", effort);
	}
}

/** Look up a model by id. */
export function getById(id: string): ModelInfo | null {
	return models.value.find((m) => m.id === id) || null;
}

export const modelStore = {
	models,
	selectedModelId,
	selectedModel,
	reasoningEffort,
	supportedReasoningEfforts,
	reasoningEffortForModel,
	setAll,
	fetch,
	select,
	setReasoningEffort,
	getById,
};
