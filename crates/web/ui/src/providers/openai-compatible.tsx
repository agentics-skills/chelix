import { render } from "preact";
import { fetchModels } from "../models";
import * as S from "../state";
import type { ProviderInfo } from "../types/model";
import { CustomProviderForm } from "./custom-provider-form";
import { closeProviderModal, els } from "./shared";

let formHost: HTMLElement | null = null;

export function showCustomProviderEditor(provider: ProviderInfo | null): void {
	const modal = els();
	modal.modal.classList.remove("hidden");
	if (formHost) render(null, formHost);
	modal.title.textContent = provider ? provider.displayName : "OpenAI Compatible";
	modal.body.textContent = "";
	formHost = document.createElement("div");
	modal.body.appendChild(formHost);
	render(
		<CustomProviderForm
			provider={provider}
			onCancel={closeCustomProviderEditor}
			onSaved={() => {
				fetchModels();
				S.refreshProvidersPage?.();
				closeCustomProviderEditor();
			}}
		/>,
		formHost,
	);
}

export function unmountCustomProviderEditor(): void {
	if (!formHost) return;
	render(null, formHost);
	formHost = null;
}

export function closeCustomProviderEditor(): void {
	unmountCustomProviderEditor();
	closeProviderModal();
}
