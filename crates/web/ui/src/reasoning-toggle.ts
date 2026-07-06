// ── Reasoning effort toggle ──────────────────────────────────
//
// Adds a "brain" combo next to the model selector that lets users
// pick Low / Medium / High reasoning effort for models that support
// extended thinking. The selected effort is sent as an explicit
// `reasoningEffort` value alongside the selected model.

import { effect } from "@preact/signals";
import { sendRpc } from "./helpers";
import { t } from "./i18n";
import * as S from "./state";
import { modelStore } from "./stores/model-store";

const EFFORT_VALUES: string[] = ["", "none", "minimal", "low", "medium", "high", "xhigh", "max"];

let reasoningCombo: HTMLElement | null = null;
let reasoningComboBtn: HTMLElement | null = null;
let reasoningComboLabel: HTMLElement | null = null;
let reasoningDropdown: HTMLElement | null = null;
let reasoningDropdownList: HTMLElement | null = null;
let disposeVisibility: (() => void) | null = null;

function effortLabel(effort: string): string {
	const map: Record<string, string> = {
		"": t("chat:reasoningOff"),
		none: t("chat:reasoningNone"),
		minimal: t("chat:reasoningMinimal"),
		low: t("chat:reasoningLow"),
		medium: t("chat:reasoningMedium"),
		high: t("chat:reasoningHigh"),
		xhigh: t("chat:reasoningExtraHigh"),
		max: t("chat:reasoningMax"),
	};
	return map[effort] ?? t("chat:reasoningOff");
}

function renderOptions(): void {
	if (!reasoningDropdownList) return;
	reasoningDropdownList.textContent = "";
	const current = modelStore.reasoningEffort.value;
	for (const value of EFFORT_VALUES) {
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
	modelStore.setReasoningEffort(effort);
	sendRpc("sessions.patch", { key: S.activeSessionKey, reasoningEffort: effort });
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
		reasoningCombo?.classList.toggle("hidden", !show);
		// Reset effort only when the selected model is resolved in the loaded
		// model list and genuinely lacks reasoning support. During initial
		// page load the list is still empty, so supportsReasoning is false
		// merely because the model is unresolved yet — wiping the restored
		// effort here made the toggle always show "Off" on chat open.
		const modelResolved = modelStore.selectedModel.value !== null;
		if (!show && modelResolved && modelStore.reasoningEffort.value) {
			modelStore.setReasoningEffort("");
		}
		if (reasoningComboLabel) {
			reasoningComboLabel.textContent = effortLabel(modelStore.reasoningEffort.value);
		}
	});
}

/** Restore reasoning toggle state from a session's stored reasoning effort. */
export function restoreReasoningEffort(storedEffort?: string | null): void {
	modelStore.setReasoningEffort(storedEffort || "");
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
