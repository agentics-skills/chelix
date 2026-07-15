// ── Custom OpenAI-compatible provider form ───────────────────

import { sendRpc } from "../helpers";
import { providerBaseUrlError, validateProviderKey } from "../provider-validation";
import type { RpcResponse } from "../types";
import { showModelSelector } from "./auth-flow";
import {
	bindValidationProgressEvents,
	completeValidationProgress,
	createValidationProgress,
	createValidationRequestId,
	els,
	openProviderModal,
	resetValidationProgress,
	setFormError,
	setValidationProgress,
} from "./shared";
import type { AddCustomPayload, ModelEntry, ProviderInfo } from "./types";

export function showCustomProviderForm(): void {
	const m = els();
	m.title.textContent = "OpenAI Compatible";
	m.body.textContent = "";

	const form = document.createElement("div");
	form.className = "provider-key-form";

	// Endpoint URL
	const urlLabel = document.createElement("label");
	urlLabel.className = "text-xs text-[var(--muted)]";
	urlLabel.textContent = "Endpoint URL";
	form.appendChild(urlLabel);

	const urlInp = document.createElement("input");
	urlInp.className = "provider-key-input";
	urlInp.type = "text";
	urlInp.placeholder = "https://api.example.com/v1";
	form.appendChild(urlInp);

	// API Key
	const keyLabel = document.createElement("label");
	keyLabel.className = "text-xs text-[var(--muted)] mt-2";
	keyLabel.textContent = "API Key";
	form.appendChild(keyLabel);

	const keyInp = document.createElement("input");
	keyInp.className = "provider-key-input";
	keyInp.type = "password";
	keyInp.placeholder = "sk-...";
	form.appendChild(keyInp);

	const errorPanel = document.createElement("div");
	errorPanel.className = "alert-error-text text-[var(--error)] whitespace-pre-line";
	errorPanel.style.display = "none";
	form.appendChild(errorPanel);

	const validationProgress = createValidationProgress(form, "mt-1");

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
	saveBtn.textContent = "Add Provider";
	saveBtn.addEventListener("click", () => {
		const url = urlInp.value.trim();
		const key = keyInp.value.trim();

		if (!url) {
			setFormError(errorPanel, "Endpoint URL is required.");
			return;
		}
		if (!key) {
			setFormError(errorPanel, "API key is required.");
			return;
		}
		const endpointError = providerBaseUrlError(url);
		if (endpointError) {
			setFormError(errorPanel, endpointError);
			return;
		}

		saveBtn.disabled = true;
		saveBtn.textContent = "Adding...";
		setValidationProgress(validationProgress, 8, "Saving provider settings...");
		setFormError(errorPanel, null);

		sendRpc<AddCustomPayload>("providers.add_custom", { baseUrl: url, apiKey: key })
			.then((res: RpcResponse<AddCustomPayload>) => {
				if (!res?.ok) {
					saveBtn.disabled = false;
					saveBtn.textContent = "Add Provider";
					resetValidationProgress(validationProgress);
					setFormError(errorPanel, res?.error?.message || "Failed to add provider.");
					return;
				}
				const result = res.payload as AddCustomPayload;
				const providerName = result.providerName;
				const displayName = result.displayName;
				const requestId = createValidationRequestId();
				setValidationProgress(validationProgress, 12, "Discovering models...");
				const stopProgressEvents = bindValidationProgressEvents(validationProgress, requestId);

				// Validate the provider to discover models
				validateProviderKey(providerName, key, url, requestId)
					.then((valResult) => {
						if (!valResult.valid) {
							saveBtn.disabled = false;
							saveBtn.textContent = "Add Provider";
							resetValidationProgress(validationProgress);
							setFormError(errorPanel, valResult.error || "No models with complete metadata were discovered.");
							return;
						}

						if (valResult.models && valResult.models.length > 0) {
							completeValidationProgress(validationProgress, "Done.");
							// Show model selector
							const customProvider: ProviderInfo = {
								name: providerName,
								displayName: displayName,
								authType: "api-key",
								configured: true,
								defaultBaseUrl: url,
								baseUrl: url,
								models: {},
								requiresModel: true,
								keyOptional: false,
								isCustom: true,
								uiOrder: 0,
							};
							showModelSelector(customProvider, valResult.models as ModelEntry[], key, url, true);
						} else {
							saveBtn.disabled = false;
							saveBtn.textContent = "Add Provider";
							resetValidationProgress(validationProgress);
							setFormError(errorPanel, "No models with complete metadata were discovered.");
						}
					})
					.catch((err: Error) => {
						saveBtn.disabled = false;
						saveBtn.textContent = "Add Provider";
						resetValidationProgress(validationProgress);
						setFormError(errorPanel, err?.message || "Validation failed.");
					})
					.finally(() => {
						stopProgressEvents();
					});
			})
			.catch((err: Error) => {
				saveBtn.disabled = false;
				saveBtn.textContent = "Add Provider";
				resetValidationProgress(validationProgress);
				setFormError(errorPanel, err?.message || "Failed to add provider.");
			});
	});
	btns.appendChild(saveBtn);
	form.appendChild(btns);
	m.body.appendChild(form);
	urlInp.focus();
}
