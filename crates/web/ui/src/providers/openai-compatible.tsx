import { render } from "preact";
import { fetchModels } from "../models";
import * as S from "../state";
import type { ProviderInfo } from "../types/model";
import { OpenAiCompatibleForm } from "./openai-compatible-form";
import { closeProviderModal, els } from "./shared";

let formHost: HTMLElement | null = null;

export function showOpenAiCompatibleEditor(provider: ProviderInfo | null): void {
	const modal = els();
	modal.modal.classList.remove("hidden");
	if (formHost) render(null, formHost);
	modal.title.textContent = provider ? provider.displayName : "OpenAI Compatible";
	modal.body.textContent = "";
	formHost = document.createElement("div");
	modal.body.appendChild(formHost);
	render(
		<OpenAiCompatibleForm
			provider={provider}
			onCancel={closeOpenAiCompatibleEditor}
			onSaved={() => {
				fetchModels();
				S.refreshProvidersPage?.();
				closeOpenAiCompatibleEditor();
			}}
		/>,
		formHost,
	);
}

export function unmountOpenAiCompatibleEditor(): void {
	if (!formHost) return;
	render(null, formHost);
	formHost = null;
}

export function closeOpenAiCompatibleEditor(): void {
	unmountOpenAiCompatibleEditor();
	closeProviderModal();
}
