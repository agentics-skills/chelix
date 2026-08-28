// ── Reasoning effort toggle ──────────────────────────────────
//
// Adds a "brain" combo next to the model selector for choosing one exact
// provider-defined effort. The selected effort is sent together with the model.

import { effect } from "@preact/signals";
import { t } from "./i18n";
import { requireSessionModelState, setSessionModel } from "./models";
import * as S from "./state";
import { modelStore } from "./stores/model-store";

let reasoningCombo: HTMLElement | null = null;
let reasoningComboBtn: HTMLElement | null = null;
let reasoningComboLabel: HTMLElement | null = null;
let reasoningDropdown: HTMLElement | null = null;
let reasoningDropdownList: HTMLElement | null = null;
let disposeVisibility: (() => void) | null = null;

function effortLabel(effort: string | null): string {
	return effort ?? t("chat:reasoningSelect");
}

function renderOptions(): void {
	if (!reasoningDropdownList) return;
	reasoningDropdownList.textContent = "";
	const current = modelStore.reasoningEffort.value;
	for (const value of modelStore.supportedReasoningEfforts.value) {
		const el = document.createElement("div");
		el.className = "model-dropdown-item";
		if (value === current) el.classList.add("selected");
		const label = document.createElement("span");
		label.className = "model-item-label";
		label.textContent = effortLabel(value);
		el.appendChild(label);
		el.addEventListener("click", selectEffort.bind(null, value));
		reasoningDropdownList.appendChild(el);
	}
}

function selectEffort(effort: string): void {
	const model = modelStore.selectedModel.value;
	if (!(model && requireSessionModelState(S.activeSessionKey))) return;
	modelStore.setReasoningEffort(effort);
	void setSessionModel(S.activeSessionKey, {
		model: model.id,
		reasoningEffort: effort,
	});
	if (reasoningComboLabel) reasoningComboLabel.textContent = effortLabel(effort);
	closeDropdown();
}

function openDropdown(): void {
	if (!reasoningDropdown) return;
	renderOptions();
	reasoningDropdown.classList.remove("hidden");
}

function closeDropdown(): void {
	if (!reasoningDropdown) return;
	reasoningDropdown.classList.add("hidden");
}

function handleOutsideClick(e: MouseEvent): void {
	if (reasoningCombo && !reasoningCombo.contains(e.target as Node)) {
		closeDropdown();
	}
}

export function bindReasoningToggle(): void {
	reasoningCombo = document.getElementById("reasoningCombo");
	reasoningComboBtn = document.getElementById("reasoningComboBtn");
	reasoningComboLabel = document.getElementById("reasoningComboLabel");
	reasoningDropdown = document.getElementById("reasoningDropdown");
	reasoningDropdownList = document.getElementById("reasoningDropdownList");
	if (!(reasoningCombo && reasoningComboBtn && reasoningDropdownList)) return;

	reasoningComboBtn.addEventListener("click", () => {
		if (reasoningDropdown?.classList.contains("hidden")) {
			openDropdown();
		} else {
			closeDropdown();
		}
	});

	document.addEventListener("click", handleOutsideClick);

	// Reactively show/hide the combo based on model reasoning support
	disposeVisibility = effect(() => {
		const show = modelStore.supportsReasoning.value;
		const supportedEfforts = modelStore.supportedReasoningEfforts.value;
		reasoningCombo?.classList.toggle("hidden", !show);
		const selectedEffort = modelStore.reasoningEffort.value;
		const displayedEffort =
			selectedEffort !== null && supportedEfforts.includes(selectedEffort) ? selectedEffort : null;
		if (reasoningComboLabel) {
			reasoningComboLabel.textContent = effortLabel(displayedEffort);
		}
	});
}

/** Restore reasoning toggle state from a session's stored reasoning effort. */
export function restoreReasoningEffort(storedEffort?: string | null): void {
	modelStore.setReasoningEffort(storedEffort ?? null);
	if (reasoningComboLabel) {
		reasoningComboLabel.textContent = effortLabel(modelStore.reasoningEffort.value);
	}
}

export function unbindReasoningToggle(): void {
	document.removeEventListener("click", handleOutsideClick);
	disposeVisibility?.();
	disposeVisibility = null;
	reasoningCombo = null;
	reasoningComboBtn = null;
	reasoningComboLabel = null;
	reasoningDropdown = null;
	reasoningDropdownList = null;
}
