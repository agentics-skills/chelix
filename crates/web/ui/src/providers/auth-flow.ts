// ── Provider API key flow and model selector ─────────────────

import { sendRpc } from "../helpers";
import { fetchModels } from "../models";
import { providerApiKeyHelp } from "../provider-key-help";
import type { TestModelResult } from "../provider-validation";
import {
	humanizeProbeError,
	isModelServiceNotConfigured,
	isTimeoutError,
	providerBaseUrlError,
	saveProviderKey,
	testModel,
	validateProviderKey,
} from "../provider-validation";
import * as S from "../state";
import { modelConfigMapFromSelection, selectedModelIdsFromConfig } from "../types/model";
import type { RpcResponse } from "../types/rpc";
import {
	bindValidationProgressEvents,
	closeProviderModal,
	completeValidationProgress,
	createValidationProgress,
	createValidationRequestId,
	els,
	OPENAI_COMPATIBLE_PROVIDERS,
	openProviderModal,
	resetValidationProgress,
	setFormError,
	setValidationProgress,
	shouldUseCustomProviderForOpenAi,
} from "./shared";
import type { AddCustomPayload, ModelEntry, ModelSelectorWrapper, ProbeResult, ProviderInfo } from "./types";

// ── API key form ─────────────────────────────────────────────

export function showApiKeyForm(provider: ProviderInfo): void {
	const m = els();
	m.title.textContent = provider.displayName;
	m.body.textContent = "";

	const form = document.createElement("div");
	form.className = "provider-key-form";

	// Check if this provider supports custom endpoint
	const supportsEndpoint = OPENAI_COMPATIBLE_PROVIDERS.includes(provider.name);

	// API Key field
	const keyLabel = document.createElement("label");
	keyLabel.className = "text-xs text-[var(--muted)]";
	keyLabel.textContent = "API Key";
	form.appendChild(keyLabel);

	const keyInp = document.createElement("input");
	keyInp.className = "provider-key-input";
	keyInp.type = "password";
	keyInp.placeholder = provider.keyOptional ? "(optional)" : "sk-...";
	form.appendChild(keyInp);

	const errorPanel = document.createElement("div");
	errorPanel.className = "alert-error-text text-[var(--error)] whitespace-pre-line";
	errorPanel.style.display = "none";
	form.appendChild(errorPanel);

	const keyHelp = providerApiKeyHelp(provider as Parameters<typeof providerApiKeyHelp>[0]);
	if (keyHelp) {
		const keyHelpLine = document.createElement("div");
		keyHelpLine.className = "text-xs text-[var(--muted)] mt-1";
		if (keyHelp.url) {
			keyHelpLine.append(`${keyHelp.text} `);
			const keyLink = document.createElement("a");
			keyLink.href = keyHelp.url;
			keyLink.target = "_blank";
			keyLink.rel = "noopener noreferrer";
			keyLink.className = "text-[var(--accent)] underline";
			keyLink.textContent = keyHelp.label || keyHelp.url;
			keyHelpLine.appendChild(keyLink);
		} else {
			keyHelpLine.textContent = keyHelp.text;
		}
		form.appendChild(keyHelpLine);
	}

	// Endpoint field for OpenAI-compatible providers
	let endpointInp: HTMLInputElement | null = null;
	if (supportsEndpoint) {
		const endpointLabel = document.createElement("label");
		endpointLabel.className = "text-xs text-[var(--muted)]";
		endpointLabel.style.marginTop = "8px";
		endpointLabel.textContent = "Endpoint (optional)";
		form.appendChild(endpointLabel);

		endpointInp = document.createElement("input");
		endpointInp.className = "provider-key-input";
		endpointInp.type = "text";
		endpointInp.placeholder = provider.defaultBaseUrl || "https://api.example.com/v1";
		form.appendChild(endpointInp);

		const hint = document.createElement("div");
		hint.className = "text-xs text-[var(--muted)]";
		hint.style.marginTop = "2px";
		hint.textContent = "Leave empty to use the default endpoint.";
		form.appendChild(hint);
	}

	const validationProgress = createValidationProgress(form, "mt-2");

	const btns = document.createElement("div");
	btns.className = "btn-row";
	btns.style.marginTop = "12px";

	const backBtn = document.createElement("button");
	backBtn.className = "provider-btn provider-btn-secondary";
	backBtn.textContent = "Back";
	backBtn.addEventListener("click", openProviderModal);
	btns.appendChild(backBtn);

	const saveBtn = document.createElement("button");
	saveBtn.className = "provider-btn";
	saveBtn.textContent = "Save";
	saveBtn.addEventListener("click", () => {
		const key = keyInp.value.trim();
		if (!(key || provider.keyOptional)) {
			setFormError(errorPanel, "API key is required.");
			return;
		}

		saveBtn.disabled = true;
		saveBtn.textContent = "Saving...";
		setValidationProgress(validationProgress, 10, "Discovering models...");
		setFormError(errorPanel, null);

		const keyVal = key || provider.name;
		const endpointVal = endpointInp?.value.trim() || null;
		const endpointError = providerBaseUrlError(endpointVal);
		if (endpointError) {
			saveBtn.disabled = false;
			saveBtn.textContent = "Save";
			resetValidationProgress(validationProgress);
			setFormError(errorPanel, endpointError);
			return;
		}
		const requestId = createValidationRequestId();
		const stopProgressEvents = bindValidationProgressEvents(validationProgress, requestId);

		validateProviderKey(provider.name, keyVal, endpointVal, requestId)
			.then((result) => {
				if (!result.valid) {
					saveBtn.disabled = false;
					saveBtn.textContent = "Save";
					resetValidationProgress(validationProgress);
					setFormError(errorPanel, result.error || "Failed to connect. Please check your credentials.");
					return;
				}

				const models = result.models || [];
				completeValidationProgress(validationProgress, "Done.");
				showModelSelector(provider, models, keyVal, endpointVal);
			})
			.catch((err: Error) => {
				saveBtn.disabled = false;
				saveBtn.textContent = "Save";
				resetValidationProgress(validationProgress);
				setFormError(errorPanel, err?.message || "Failed to connect.");
			})
			.finally(() => {
				stopProgressEvents();
			});
	});
	btns.appendChild(saveBtn);
	form.appendChild(btns);
	m.body.appendChild(form);
	keyInp.focus();
}

// ── Model selector (after auth) ──────────────────────────────

export function showModelSelector(
	provider: ProviderInfo,
	models: ModelEntry[],
	keyVal: string | null,
	endpointVal: string | null,
	skipSave?: boolean,
): void {
	const m = els();
	m.title.textContent = `${provider.displayName} \u2014 Select Models`;
	m.body.textContent = "";

	const selectedIds: Set<string> = new Set();

	const wrapper = document.createElement("div") as ModelSelectorWrapper;
	wrapper.className = "provider-key-form flex flex-col min-h-0 flex-1";

	const label = document.createElement("div");
	label.className = "text-xs font-medium text-[var(--text-strong)] mb-1 shrink-0";
	label.textContent = "Select models to add";
	wrapper.appendChild(label);

	const hint = document.createElement("div");
	hint.className = "text-xs text-[var(--muted)] mb-2 shrink-0";
	hint.textContent = "Click models to toggle selection, or use Select All.";
	wrapper.appendChild(hint);

	// Search + Select All row when >5 models
	let searchInp: HTMLInputElement | null = null;
	if (models.length > 5) {
		searchInp = document.createElement("input");
		searchInp.type = "text";
		searchInp.className = "provider-key-input w-full text-xs mb-2 shrink-0";
		searchInp.placeholder = "Search models\u2026";
		wrapper.appendChild(searchInp);
	}

	const selectAllBtn = document.createElement("button");
	selectAllBtn.className = "provider-btn provider-btn-secondary text-xs mb-2 shrink-0";

	function getVisibleModels(): ModelEntry[] {
		const currentFilter = searchInp?.value.trim() || null;
		if (!currentFilter) return models;
		const q = currentFilter.toLowerCase();
		return models.filter(
			(mdl: ModelEntry) => mdl.display_name.toLowerCase().includes(q) || mdl.id.toLowerCase().includes(q),
		);
	}

	function updateSelectAllLabel(): void {
		const visible = getVisibleModels();
		const allVisible = visible.length > 0 && visible.every((mdl: ModelEntry) => selectedIds.has(mdl.id));
		selectAllBtn.textContent = allVisible ? "Deselect All" : "Select All";
	}
	updateSelectAllLabel();

	selectAllBtn.addEventListener("click", () => {
		const visible = getVisibleModels();
		const allVisible = visible.every((mdl: ModelEntry) => selectedIds.has(mdl.id));
		if (allVisible) {
			for (const mdl of visible) selectedIds.delete(mdl.id);
		} else {
			for (const visibleModel of visible) selectedIds.add(visibleModel.id);
		}
		updateSelectAllLabel();
		updateStatus();
		renderCards(searchInp?.value.trim() || null);
	});
	wrapper.appendChild(selectAllBtn);

	const list = document.createElement("div");
	list.className = "flex flex-col gap-1 overflow-y-auto flex-1 min-h-0 max-h-56";
	wrapper.appendChild(list);

	const statusArea = document.createElement("div");
	statusArea.className = "text-xs text-[var(--muted)] mt-2 shrink-0";
	wrapper.appendChild(statusArea);

	function updateStatus(): void {
		const count = selectedIds.size;
		statusArea.textContent = count === 0 ? "No models selected" : `${count} model${count > 1 ? "s" : ""} selected`;
	}

	const errorArea = document.createElement("div");
	errorArea.className = "alert-error-text text-[var(--error)] whitespace-pre-line shrink-0";
	errorArea.style.display = "none";
	wrapper.appendChild(errorArea);

	function renderCards(filter: string | null): void {
		list.textContent = "";
		let filtered = models;
		if (filter) {
			const q = filter.toLowerCase();
			filtered = models.filter(
				(mdl: ModelEntry) => mdl.display_name.toLowerCase().includes(q) || mdl.id.toLowerCase().includes(q),
			);
		}
		if (filtered.length === 0) {
			const empty = document.createElement("div");
			empty.className = "text-xs text-[var(--muted)] py-4 text-center";
			empty.textContent = "No models match your search.";
			list.appendChild(empty);
			return;
		}
		filtered.forEach((mdl: ModelEntry) => {
			const card = document.createElement("div");
			card.className = `model-card ${selectedIds.has(mdl.id) ? "selected" : ""}`;

			const header = document.createElement("div");
			header.className = "flex items-center justify-between";

			const name = document.createElement("span");
			name.className = "text-sm font-medium text-[var(--text)]";
			name.textContent = mdl.display_name;
			header.appendChild(name);

			const badges = document.createElement("div");
			badges.className = "flex gap-2";

			if (mdl.tool_calling) {
				const toolsBadge = document.createElement("span");
				toolsBadge.className = "recommended-badge";
				toolsBadge.textContent = "Tools";
				badges.appendChild(toolsBadge);
			}

			header.appendChild(badges);
			card.appendChild(header);

			const idLine = document.createElement("div");
			idLine.className = "text-xs text-[var(--muted)] mt-1 font-mono";
			idLine.textContent = mdl.id;
			card.appendChild(idLine);

			((modelId: string) => {
				card.addEventListener("click", () => {
					if (selectedIds.has(modelId)) {
						selectedIds.delete(modelId);
					} else {
						selectedIds.add(modelId);
					}
					updateSelectAllLabel();
					updateStatus();
					renderCards(searchInp?.value.trim() || null);
				});
			})(mdl.id);

			list.appendChild(card);
		});
	}

	renderCards(null);
	updateStatus();

	if (searchInp) {
		searchInp.addEventListener("input", () => {
			renderCards(searchInp?.value.trim());
		});
	}

	// Buttons
	const btns = document.createElement("div");
	btns.className = "btn-row mt-3 shrink-0";

	const backBtn = document.createElement("button");
	backBtn.className = "provider-btn provider-btn-secondary";
	backBtn.textContent = "Back";
	backBtn.addEventListener("click", () => {
		if (skipSave) {
			openProviderModal();
		} else {
			showApiKeyForm(provider);
		}
	});
	btns.appendChild(backBtn);

	const continueBtn = document.createElement("button");
	continueBtn.className = "provider-btn";
	continueBtn.textContent = "Continue";
	continueBtn.addEventListener("click", () => {
		if (selectedIds.size === 0) {
			errorArea.textContent = "Select at least one model to continue.";
			errorArea.style.display = "";
			return;
		}
		errorArea.style.display = "none";
		continueBtn.disabled = true;
		continueBtn.textContent = "Saving\u2026";
		saveAndFinishProvider(provider, models, keyVal, endpointVal, selectedIds, !!skipSave);
	});
	btns.appendChild(continueBtn);

	wrapper.appendChild(btns);

	// Expose error area for saveAndFinishProvider to use
	wrapper._errorArea = errorArea;
	wrapper._resetSelection = () => {
		continueBtn.disabled = false;
		continueBtn.textContent = "Continue";
		renderCards(searchInp?.value.trim() || null);
	};

	m.body.appendChild(wrapper);
}

// ── Save and finish provider ─────────────────────────────────

type ProviderModelConfig = ReturnType<typeof modelConfigMapFromSelection>;

interface ProviderSaveRequest {
	provider: ProviderInfo;
	modelsForSave: ProviderModelConfig;
	modelIds: string[];
	keyVal: string | null;
	endpointVal: string | null;
	skipSave: boolean;
	saveAsCustomProvider: boolean;
}

interface SavedProviderIdentity {
	name: string;
	displayName: string;
}

type ProviderModelProbe =
	| { status: "skipped" }
	| { status: "ready"; modelId: string }
	| { status: "service-unavailable"; modelId: string }
	| { status: "timeout"; modelId: string }
	| { status: "failed"; error: string };

function showProviderSaveError(message: string): void {
	const wrapper = els().body.querySelector(".provider-key-form") as ModelSelectorWrapper | null;
	if (!wrapper?._errorArea) return;
	setFormError(wrapper._errorArea, message);
	wrapper._resetSelection?.();
}

function saveProviderCredentials(request: ProviderSaveRequest): Promise<RpcResponse> {
	if (request.skipSave) return Promise.resolve({ ok: true });
	if (request.saveAsCustomProvider) {
		return sendRpc("providers.add_custom", {
			baseUrl: request.endpointVal,
			apiKey: request.keyVal,
			models: request.modelsForSave,
		});
	}
	return saveProviderKey(request.provider.name, request.keyVal || "", request.endpointVal);
}

function savedProviderIdentity(response: RpcResponse, request: ProviderSaveRequest): SavedProviderIdentity {
	if (!request.saveAsCustomProvider) {
		return { name: request.provider.name, displayName: request.provider.displayName };
	}
	const payload = response.payload as AddCustomPayload | undefined;
	return {
		name: payload?.providerName || request.provider.name,
		displayName: payload?.displayName || request.provider.displayName,
	};
}

function firstModelTestId(request: ProviderSaveRequest, providerName: string): string | null {
	const firstModelId = request.modelIds[0];
	if (!firstModelId) return null;
	if (!request.saveAsCustomProvider) return firstModelId;
	const firstRawModelId = Object.keys(request.modelsForSave)[0];
	if (!firstRawModelId) throw new Error("Selected model is missing from the provider model catalog.");
	return `${providerName}::${firstRawModelId}`;
}

function classifyProviderModelProbe(modelId: string, result: TestModelResult): ProviderModelProbe {
	if (result.ok) return { status: "ready", modelId };
	const error = result.error || "";
	if (isModelServiceNotConfigured(error)) return { status: "service-unavailable", modelId };
	if (isTimeoutError(error)) return { status: "timeout", modelId };
	return { status: "failed", error: error || "Model test failed. Try another model." };
}

async function probeFirstProviderModel(
	request: ProviderSaveRequest,
	providerName: string,
): Promise<ProviderModelProbe> {
	const modelId = firstModelTestId(request, providerName);
	if (!modelId) return { status: "skipped" };
	return classifyProviderModelProbe(modelId, await testModel(modelId));
}

function saveSelectedProviderModels(providerName: string, modelsForSave: ProviderModelConfig): Promise<RpcResponse> {
	return sendRpc("providers.save_models", { provider: providerName, models: modelsForSave });
}

function reportAcceptedProviderProbe(probe: ProviderModelProbe): void {
	if (probe.status === "timeout") {
		console.warn(
			"models.test timed out for",
			probe.modelId,
			"\u2014 saving models anyway (local servers may need longer to load)",
		);
	}
	if (probe.status === "service-unavailable") {
		console.warn("models.test unavailable in provider settings, saved selected models without probe");
	}
	if (probe.status !== "skipped" && probe.status !== "failed") {
		localStorage.setItem("chelix-model", probe.modelId);
	}
}

function renderProviderSaveSuccess(identity: SavedProviderIdentity, modelCount: number, modelTimedOut: boolean): void {
	const body = els().body;
	body.textContent = "";
	const status = document.createElement("div");
	status.className = "provider-status";
	const countMessage = modelCount > 1 ? ` with ${modelCount} models` : "";
	status.textContent = `${identity.displayName} configured successfully${countMessage}!`;
	body.appendChild(status);
	if (modelTimedOut) {
		const slowHint = document.createElement("div");
		slowHint.className = "text-xs text-[var(--muted)] mt-1";
		slowHint.textContent = "Note: model was slow to respond. It may need a moment to finish loading.";
		body.appendChild(slowHint);
	}
	fetchModels();
	S.refreshProvidersPage?.();
	setTimeout(closeProviderModal, modelTimedOut ? 3500 : 1500);
}

async function completeProviderSave(request: ProviderSaveRequest): Promise<void> {
	const credentialsResponse = await saveProviderCredentials(request);
	if (!credentialsResponse?.ok) {
		showProviderSaveError(credentialsResponse?.error?.message || "Failed to save credentials.");
		return;
	}
	const identity = savedProviderIdentity(credentialsResponse, request);
	const probe = await probeFirstProviderModel(request, identity.name);
	if (probe.status === "failed") {
		showProviderSaveError(probe.error);
		return;
	}
	if (probe.status !== "skipped") {
		const modelsResponse = await saveSelectedProviderModels(identity.name, request.modelsForSave);
		if (!modelsResponse?.ok) {
			showProviderSaveError(modelsResponse?.error?.message || "Failed to save models.");
			return;
		}
		reportAcceptedProviderProbe(probe);
	}
	renderProviderSaveSuccess(identity, request.modelIds.length, probe.status === "timeout");
}

function saveAndFinishProvider(
	provider: ProviderInfo,
	models: ModelEntry[],
	keyVal: string | null,
	endpointVal: string | null,
	selectedModelIds: Set<string>,
	skipSave: boolean,
): void {
	const request: ProviderSaveRequest = {
		provider,
		modelsForSave: modelConfigMapFromSelection(models, selectedModelIds),
		modelIds: Array.from(selectedModelIds),
		keyVal,
		endpointVal,
		skipSave,
		saveAsCustomProvider: !skipSave && shouldUseCustomProviderForOpenAi(provider, endpointVal),
	};
	completeProviderSave(request).catch((error: Error) => {
		showProviderSaveError(error.message || "Failed to save credentials.");
	});
}

// ── Model selector for existing providers (multi-select) ─────

export function openModelSelectorForProvider(providerName: string, providerDisplayName: string): void {
	const m = els();
	m.modal.classList.remove("hidden");
	m.title.textContent = `${providerDisplayName} \u2014 Preferred Models`;
	m.body.textContent = "Loading models...";

	Promise.all([sendRpc<ModelEntry[]>("models.list", {}), sendRpc<ProviderInfo[]>("providers.available", {})]).then(
		([modelsRes, providersRes]: [RpcResponse<ModelEntry[]>, RpcResponse<ProviderInfo[]>]) => {
			const allModels: ModelEntry[] = modelsRes?.ok ? (modelsRes.payload as ModelEntry[]) || [] : [];
			const provModels = allModels.filter((entry: ModelEntry) => entry.provider === providerName);

			if (provModels.length === 0) {
				m.body.textContent = "";
				const wrapper = document.createElement("div");
				wrapper.className = "provider-key-form";
				const msg = document.createElement("div");
				msg.className = "text-xs text-[var(--muted)] py-4 text-center";
				msg.textContent = "No models available yet. Try running Detect All Models first.";
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

			// Get saved preferred models for this provider.
			let savedModels = new Set<string>();
			if (providersRes?.ok) {
				const providerMeta = ((providersRes.payload as ProviderInfo[]) || []).find(
					(p: ProviderInfo) => p.name === providerName,
				);
				if (providerMeta) savedModels = selectedModelIdsFromConfig(provModels, providerMeta.models);
			}

			showMultiModelSelector(providerName, providerDisplayName, provModels, savedModels);
		},
	);
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

	type ModelProbeState = "probing" | "ok" | ProbeResult;
	const probeResults = new Map<string, ModelProbeState>();

	function applyModelProbeResult(modelId: string, result: TestModelResult): void {
		const error = result.error || "";
		if (isModelServiceNotConfigured(error)) {
			probeResults.delete(modelId);
			return;
		}
		if (!result.ok && isTimeoutError(error)) {
			probeResults.set(modelId, { error: "Slow to respond (may still work)", timeout: true });
			return;
		}
		probeResults.set(modelId, result.ok ? "ok" : { error: humanizeProbeError(error || "Unsupported") as string });
	}

	function probeModel(modelId: string): void {
		if (probeResults.has(modelId)) return;
		probeResults.set(modelId, "probing");
		renderCards(searchInp?.value.trim() || null);
		testModel(modelId).then((result: TestModelResult) => {
			applyModelProbeResult(modelId, result);
			renderCards(searchInp?.value.trim() || null);
		});
	}

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
			const aTime = a.created_at || 0;
			const bTime = b.created_at || 0;
			if (aTime !== bTime) return bTime - aTime;
			return a.display_name.localeCompare(b.display_name);
		});
	}

	function filteredModels(filter: string | null): ModelEntry[] {
		if (!filter) return models;
		const query = filter.toLowerCase();
		return models.filter(
			(model) => model.display_name.toLowerCase().includes(query) || model.id.toLowerCase().includes(query),
		);
	}

	function createModelBadge(className: string, text: string): HTMLSpanElement {
		const badge = document.createElement("span");
		badge.className = className;
		badge.textContent = text;
		return badge;
	}

	function failedProbe(probe: ModelProbeState | undefined): ProbeResult | null {
		return typeof probe === "object" ? probe : null;
	}

	function createModelBadges(model: ModelEntry, probe: ModelProbeState | undefined): HTMLDivElement {
		const badges = document.createElement("div");
		badges.className = "flex gap-2";
		if (model.tool_calling) badges.appendChild(createModelBadge("recommended-badge", "Tools"));
		if (probe === "probing") badges.appendChild(createModelBadge("tier-badge", "Probing\u2026"));
		const failure = failedProbe(probe);
		if (failure) {
			const className = failure.timeout ? "tier-badge" : "provider-item-badge warning";
			badges.appendChild(createModelBadge(className, failure.timeout ? "Slow" : "Unsupported"));
		}
		return badges;
	}

	function createModelHeader(model: ModelEntry, probe: ModelProbeState | undefined): HTMLDivElement {
		const header = document.createElement("div");
		header.className = "flex items-center justify-between";
		const name = document.createElement("span");
		name.className = "text-sm font-medium text-[var(--text)] truncate";
		name.textContent = model.display_name;
		header.append(name, createModelBadges(model, probe));
		return header;
	}

	function appendModelId(card: HTMLElement, modelId: string): void {
		const idLine = document.createElement("div");
		idLine.className = "text-xs text-[var(--muted)] mt-1 font-mono";
		idLine.textContent = modelId;
		card.appendChild(idLine);
	}

	function appendModelProbeError(card: HTMLElement, probe: ModelProbeState | undefined): void {
		const error = failedProbe(probe)?.error;
		if (!error) return;
		const errorLine = document.createElement("div");
		errorLine.className = "text-xs font-medium text-[var(--danger,#ef4444)] mt-0.5";
		errorLine.textContent = error;
		card.appendChild(errorLine);
	}

	function appendModelDate(card: HTMLElement, createdAt: number | null | undefined): void {
		if (!createdAt) return;
		const dateLine = document.createElement("time");
		dateLine.className = "text-xs text-[var(--muted)] mt-0.5 opacity-60 block";
		dateLine.setAttribute("data-epoch-ms", String(createdAt * 1000));
		dateLine.setAttribute("data-format", "year-month");
		card.appendChild(dateLine);
	}

	function toggleSelectedModel(modelId: string): void {
		if (selectedIds.has(modelId)) {
			selectedIds.delete(modelId);
		} else {
			selectedIds.add(modelId);
			probeModel(modelId);
		}
		renderCards(searchInp?.value.trim() || null);
		updateStatus();
	}

	function createModelCard(model: ModelEntry): HTMLDivElement {
		const card = document.createElement("div");
		card.className = `model-card ${selectedIds.has(model.id) ? "selected" : ""}`;
		const probe = probeResults.get(model.id);
		card.appendChild(createModelHeader(model, probe));
		appendModelId(card, model.id);
		appendModelProbeError(card, probe);
		appendModelDate(card, model.created_at);
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
		saveBtn.disabled = true;
		saveBtn.textContent = "Saving\u2026";
		errorArea.style.display = "none";

		const modelsForSave = modelConfigMapFromSelection(models, selectedIds);
		sendRpc("providers.save_models", { provider: providerName, models: modelsForSave })
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
