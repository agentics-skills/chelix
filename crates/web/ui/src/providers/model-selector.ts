import { sendRpc } from "../helpers";
import { fetchModels } from "../models";
import * as S from "../state";
import type { RpcResponse } from "../types/rpc";
import { closeProviderModal, els } from "./shared";
import type { ModelEntry } from "./types";

export function openModelSelectorForProvider(providerName: string, providerDisplayName: string): void {
	const m = els();
	m.modal.classList.remove("hidden");
	m.title.textContent = `${providerDisplayName} \u2014 Preferred Models`;
	m.body.textContent = "Loading models...";

	sendRpc<ModelEntry[]>("models.list", {}).then((modelsRes: RpcResponse<ModelEntry[]>) => {
		const allModels: ModelEntry[] = modelsRes?.ok ? (modelsRes.payload as ModelEntry[]) || [] : [];
		const provModels = allModels.filter((entry: ModelEntry) => entry.provider === providerName);

		if (provModels.length === 0) {
			m.body.textContent = "";
			const wrapper = document.createElement("div");
			wrapper.className = "provider-key-form";
			const msg = document.createElement("div");
			msg.className = "text-xs text-[var(--muted)] py-4 text-center";
			msg.textContent = "No configured models are available for this provider.";
			wrapper.appendChild(msg);
			const btns = document.createElement("div");
			btns.className = "btn-row mt-3";
			const closeBtn = document.createElement("button");
			closeBtn.className = "provider-btn provider-btn-secondary";
			closeBtn.textContent = "Close";
			closeBtn.addEventListener("click", closeProviderModal);
			btns.appendChild(closeBtn);
			wrapper.appendChild(btns);
			m.body.appendChild(wrapper);
			return;
		}

		const savedModels = new Set(provModels.filter((model) => model.preferred).map((model) => model.id));
		showMultiModelSelector(providerName, providerDisplayName, provModels, savedModels);
	});
}

function showMultiModelSelector(
	providerName: string,
	providerDisplayName: string,
	models: ModelEntry[],
	savedModels: Set<string>,
): void {
	const m = els();
	m.title.textContent = `${providerDisplayName} \u2014 Preferred Models`;
	m.body.textContent = "";

	const selectedIds: Set<string> = new Set(savedModels);

	const wrapper = document.createElement("div");
	wrapper.className = "provider-key-form flex flex-col min-h-0 flex-1";

	const label = document.createElement("div");
	label.className = "text-xs font-medium text-[var(--text-strong)] mb-1 shrink-0";
	label.textContent = "Select models to pin at the top of the dropdown";
	wrapper.appendChild(label);

	const hint = document.createElement("div");
	hint.className = "text-xs text-[var(--muted)] mb-2 shrink-0";
	hint.textContent = "Selected models appear first in the session model selector.";
	wrapper.appendChild(hint);

	// Search input when >5 models
	let searchInp: HTMLInputElement | null = null;
	if (models.length > 5) {
		searchInp = document.createElement("input");
		searchInp.type = "text";
		searchInp.className = "provider-key-input w-full text-xs mb-2 shrink-0";
		searchInp.placeholder = "Search models\u2026";
		wrapper.appendChild(searchInp);
	}

	const list = document.createElement("div");
	list.className = "flex flex-col gap-1 overflow-y-auto flex-1 min-h-0";
	wrapper.appendChild(list);

	const statusArea = document.createElement("div");
	statusArea.className = "text-xs text-[var(--muted)] mt-2 shrink-0";
	wrapper.appendChild(statusArea);

	function updateStatus(): void {
		const count = selectedIds.size;
		statusArea.textContent = count === 0 ? "No models selected" : `${count} model${count > 1 ? "s" : ""} selected`;
	}

	function sortModelsForSelection(items: ModelEntry[]): ModelEntry[] {
		return [...items].sort((a: ModelEntry, b: ModelEntry) => {
			const aSel = selectedIds.has(a.id) ? 0 : 1;
			const bSel = selectedIds.has(b.id) ? 0 : 1;
			if (aSel !== bSel) return aSel - bSel;
			return a.id.localeCompare(b.id);
		});
	}

	function filteredModels(filter: string | null): ModelEntry[] {
		if (!filter) return models;
		const query = filter.toLowerCase();
		return models.filter((model) => model.id.toLowerCase().includes(query));
	}

	function createModelBadge(className: string, text: string): HTMLSpanElement {
		const badge = document.createElement("span");
		badge.className = className;
		badge.textContent = text;
		return badge;
	}

	function createModelBadges(model: ModelEntry): HTMLDivElement {
		const badges = document.createElement("div");
		badges.className = "flex gap-2";
		if (model.tool_calling) badges.appendChild(createModelBadge("recommended-badge", "Tools"));
		return badges;
	}

	function createModelHeader(model: ModelEntry): HTMLDivElement {
		const header = document.createElement("div");
		header.className = "flex items-center justify-between";
		const name = document.createElement("span");
		name.className = "text-sm font-medium text-[var(--text)] truncate";
		name.textContent = model.id;
		header.append(name, createModelBadges(model));
		return header;
	}

	function appendModelId(card: HTMLElement, modelId: string): void {
		const idLine = document.createElement("div");
		idLine.className = "text-xs text-[var(--muted)] mt-1 font-mono";
		idLine.textContent = modelId;
		card.appendChild(idLine);
	}

	function toggleSelectedModel(modelId: string): void {
		if (selectedIds.has(modelId)) {
			selectedIds.delete(modelId);
		} else {
			selectedIds.add(modelId);
		}
		renderCards(searchInp?.value.trim() || null);
		updateStatus();
	}

	function createModelCard(model: ModelEntry): HTMLDivElement {
		const card = document.createElement("div");
		card.className = `model-card ${selectedIds.has(model.id) ? "selected" : ""}`;
		card.appendChild(createModelHeader(model));
		appendModelId(card, model.id);
		card.addEventListener("click", () => toggleSelectedModel(model.id));
		return card;
	}

	function renderEmptyModelList(): void {
		const empty = document.createElement("div");
		empty.className = "text-xs text-[var(--muted)] py-4 text-center";
		empty.textContent = "No models match your search.";
		list.appendChild(empty);
	}

	function renderCards(filter: string | null): void {
		list.textContent = "";
		const filtered = filteredModels(filter);
		if (filtered.length === 0) {
			renderEmptyModelList();
			return;
		}
		for (const model of sortModelsForSelection(filtered)) list.appendChild(createModelCard(model));
	}

	renderCards(null);
	updateStatus();

	if (searchInp) {
		searchInp.addEventListener("input", () => {
			renderCards(searchInp?.value.trim());
		});
	}

	const errorArea = document.createElement("div");
	errorArea.className = "alert-error-text text-[var(--error)] whitespace-pre-line shrink-0";
	errorArea.style.display = "none";
	wrapper.appendChild(errorArea);

	// Buttons -- always visible at the bottom
	const btns = document.createElement("div");
	btns.className = "btn-row mt-3 shrink-0";

	const cancelBtn = document.createElement("button");
	cancelBtn.className = "provider-btn provider-btn-secondary";
	cancelBtn.textContent = "Cancel";
	cancelBtn.addEventListener("click", closeProviderModal);
	btns.appendChild(cancelBtn);

	const saveBtn = document.createElement("button");
	saveBtn.className = "provider-btn";
	saveBtn.textContent = "Save";
	saveBtn.addEventListener("click", () => {
		if (selectedIds.size === 0) {
			errorArea.textContent = "Select at least one configured model.";
			errorArea.style.display = "";
			return;
		}
		saveBtn.disabled = true;
		saveBtn.textContent = "Saving\u2026";
		errorArea.style.display = "none";

		sendRpc("providers.set_model_preferences", {
			provider: providerName,
			modelIds: Array.from(selectedIds),
		})
			.then((res: RpcResponse) => {
				if (!res?.ok) {
					saveBtn.disabled = false;
					saveBtn.textContent = "Save";
					errorArea.textContent = res?.error?.message || "Failed to save model preferences.";
					errorArea.style.display = "";
					return;
				}
				fetchModels();
				if (S.refreshProvidersPage) S.refreshProvidersPage();
				closeProviderModal();
			})
			.catch((err: Error) => {
				saveBtn.disabled = false;
				saveBtn.textContent = "Save";
				errorArea.textContent = err?.message || "Failed to save model preferences.";
				errorArea.style.display = "";
			});
	});
	btns.appendChild(saveBtn);

	wrapper.appendChild(btns);
	m.body.appendChild(wrapper);
}
