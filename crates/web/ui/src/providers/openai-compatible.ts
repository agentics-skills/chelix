// ── OpenAI-compatible provider selection ─────────────────────

import { showApiKeyForm } from "./auth-flow";
import { els, openProviderModal } from "./shared";
import type { ProviderInfo } from "./types";

function appendBackButton(container: HTMLElement): void {
	const buttons = document.createElement("div");
	buttons.className = "btn-row";
	buttons.style.marginTop = "12px";

	const backButton = document.createElement("button");
	backButton.className = "provider-btn provider-btn-secondary";
	backButton.textContent = "Back";
	backButton.addEventListener("click", openProviderModal);
	buttons.appendChild(backButton);
	container.appendChild(buttons);
}

export function showOpenAiCompatibleForm(providers: ProviderInfo[]): void {
	const modal = els();
	modal.title.textContent = "OpenAI Compatible";
	modal.body.textContent = "";

	const customProviders = providers
		.filter((provider) => provider.isCustom)
		.sort((left, right) => left.displayName.localeCompare(right.displayName));

	const form = document.createElement("div");
	form.className = "provider-key-form";

	const hint = document.createElement("div");
	hint.className = "text-xs text-[var(--muted)]";
	hint.textContent =
		"Select a custom-* provider whose complete model records are declared in the service configuration.";
	form.appendChild(hint);

	if (customProviders.length === 0) {
		const error = document.createElement("div");
		error.className = "alert-error-text text-[var(--error)] whitespace-pre-line";
		error.textContent =
			"No custom-* provider is declared. Add the provider and its complete models to chelix.toml before saving credentials.";
		form.appendChild(error);
		appendBackButton(form);
		modal.body.appendChild(form);
		return;
	}

	const providerLabel = document.createElement("label");
	providerLabel.className = "text-xs text-[var(--muted)] mt-2";
	providerLabel.htmlFor = "openAiCompatibleProvider";
	providerLabel.textContent = "Configured provider";
	form.appendChild(providerLabel);

	const providerSelect = document.createElement("select");
	providerSelect.id = "openAiCompatibleProvider";
	providerSelect.className = "provider-key-input";
	for (const provider of customProviders) {
		const option = document.createElement("option");
		option.value = provider.name;
		option.textContent = `${provider.displayName} — ${provider.name}`;
		providerSelect.appendChild(option);
	}
	form.appendChild(providerSelect);

	const buttons = document.createElement("div");
	buttons.className = "btn-row";
	buttons.style.marginTop = "12px";

	const backButton = document.createElement("button");
	backButton.className = "provider-btn provider-btn-secondary";
	backButton.textContent = "Back";
	backButton.addEventListener("click", openProviderModal);
	buttons.appendChild(backButton);

	const continueButton = document.createElement("button");
	continueButton.className = "provider-btn";
	continueButton.textContent = "Continue";
	continueButton.addEventListener("click", () => {
		const selected = customProviders.find((provider) => provider.name === providerSelect.value);
		if (selected) showApiKeyForm(selected);
	});
	buttons.appendChild(continueButton);
	form.appendChild(buttons);

	modal.body.appendChild(form);
	providerSelect.focus();
}
