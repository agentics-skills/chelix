// ── Model store (signal-based) ──────────────────────────────
//
// Single source of truth for model data. Both Preact components
// (auto-subscribe) and imperative code (read .value) can use this.

import { computed, signal } from "@preact/signals";
import { sendRpc } from "../helpers";
import type { ModelInfo, RpcResponse } from "../types";

// ── Signals ──────────────────────────────────────────────────
export const models = signal<ModelInfo[]>([]);
export const selectedModelId = signal<string>(localStorage.getItem("chelix-model") || "");
export const reasoningEffort = signal<string>(localStorage.getItem("chelix-reasoning-effort") || "");

export const selectedModel = computed<ModelInfo | null>(() => {
	const id = selectedModelId.value;
	return models.value.find((m) => m.id === id) || null;
});

/** True when the currently selected model supports extended thinking. */
export const supportsReasoning = computed<boolean>(() => {
	const m = selectedModel.value;
	return !!m?.supportsReasoning;
});

// ── Methods ──────────────────────────────────────────────────

/** Replace the full model list (e.g. after fetch or bootstrap). */
export function setAll(arr: ModelInfo[]): void {
	models.value = arr || [];
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
		if (!found) localStorage.setItem("chelix-model", model.id);
	});
}

/** Select a model by id. Persists to localStorage. */
export function select(id: string): void {
	selectedModelId.value = id;
}

/** Set the reasoning effort level. Empty string means off. */
export function setReasoningEffort(effort: string): void {
	reasoningEffort.value = effort || "";
	localStorage.setItem("chelix-reasoning-effort", effort || "");
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
	supportsReasoning,
	setAll,
	fetch,
	select,
	setReasoningEffort,
	getById,
};
