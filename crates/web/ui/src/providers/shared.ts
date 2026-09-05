// ── Provider modal shared utilities and state ────────────────

import { ensureProviderModal } from "../modals";
import * as S from "../state";
import type { ProviderModalElements } from "./types";

let _els: ProviderModalElements | null = null;

export function els(): ProviderModalElements {
	if (!_els) {
		ensureProviderModal();
		_els = {
			modal: S.requireElement("providerModal"),
			body: S.requireElement("providerModalBody"),
			title: S.requireElement("providerModalTitle"),
			close: S.requireElement("providerModalClose"),
		};
		_els.close.addEventListener("click", closeProviderModal);
		_els.modal.addEventListener("click", (event: MouseEvent) => {
			if (event.target === _els?.modal) closeProviderModal();
		});
	}
	return _els;
}

export const OPENAI_COMPATIBLE_PROVIDERS: string[] = ["openai", "openrouter"];

// Dynamic import breaks the dependency cycle with auth-flow.ts.
export function openProviderModal(): void {
	import("./open-modal").then((module) => module.openProviderModalImpl());
}

export function closeProviderModal(): void {
	els().modal.classList.add("hidden");
}

export function setFormError(errorPanel: HTMLElement | null, message: string | null): void {
	if (!errorPanel) return;
	if (!message) {
		errorPanel.style.display = "none";
		errorPanel.textContent = "";
		return;
	}
	errorPanel.textContent = `Error: ${message}`;
	errorPanel.style.display = "";
}
