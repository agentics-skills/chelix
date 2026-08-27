const { expect, test } = require("../base-test");
const { modelRecord, navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

async function mockProviderModelContract(page, modelFixtures, providerFixtures, { saveKeyError = null } = {}) {
	await page.addInitScript(
		({ models: injectedModels, providers: injectedProviders, saveKeyError: injectedSaveKeyError }) => {
			window.__providerModelRequests = [];
			const originalSend = WebSocket.prototype.send;

			function respond(socket, id, payload) {
				queueMicrotask(() => {
					const event = new MessageEvent("message", {
						data: JSON.stringify({ type: "res", id, ok: true, payload }),
					});
					socket.onmessage?.(event);
				});
			}

			function respondError(socket, id, message) {
				queueMicrotask(() => {
					const event = new MessageEvent("message", {
						data: JSON.stringify({
							type: "res",
							id,
							ok: false,
							error: { code: "INVALID_ARGUMENT", message },
						}),
					});
					socket.onmessage?.(event);
				});
			}

			function handleProviderRequest(socket, request) {
				switch (request?.method) {
					case "models.list":
					case "models.list_all":
						respond(socket, request.id, injectedModels);
						return true;
					case "providers.available":
						respond(socket, request.id, injectedProviders);
						return true;
					case "models.test":
					case "providers.set_model_preferences":
						window.__providerModelRequests.push({ method: request.method, params: request.params || {} });
						respond(socket, request.id, {});
						return true;
					case "providers.save_key":
						window.__providerModelRequests.push({ method: request.method, params: request.params || {} });
						if (injectedSaveKeyError) {
							respondError(socket, request.id, injectedSaveKeyError);
						} else {
							respond(socket, request.id, {});
						}
						return true;
					default:
						return false;
				}
			}

			WebSocket.prototype.send = function (data) {
				try {
					const request = JSON.parse(data);
					if (handleProviderRequest(this, request)) return;
				} catch {
					// Fall through to the real WebSocket for unrelated requests.
				}
				return originalSend.call(this, data);
			};
		},
		{ models: modelFixtures, providers: providerFixtures, saveKeyError },
	);
}

async function openProvidersPage(page) {
	await navigateAndWait(page, "/settings/providers");
	await expect.poll(() => new URL(page.url()).pathname).toBe("/settings/providers");
	await expect(page.locator("#providersTitle")).toBeVisible();
}

async function openProviderPicker(page) {
	await waitForWsConnected(page);
	await page.locator("#providersAddLlmBtn").click();
	await expect(page.locator("#providerModal")).toBeVisible();
	const providerItems = page.locator("#providerModalBody .provider-item");
	await expect(providerItems.first()).toBeVisible();
	return providerItems;
}

function apiKeyProviderItems(page) {
	return page.locator("#providerModalBody .provider-item").filter({
		has: page.locator("#providerModalBody .provider-item-badge", { hasText: /^API Key$/ }),
	});
}

async function openApiKeyProviderForm(page) {
	const items = apiKeyProviderItems(page);
	if ((await items.count()) === 0) return false;
	await items.first().click();
	await expect(page.getByRole("button", { name: "Save", exact: true })).toBeVisible();
	return true;
}

async function openRequiredApiKeyProviderForm(page) {
	const items = apiKeyProviderItems(page);
	const count = await items.count();
	for (let index = 0; index < count; index++) {
		await items.nth(index).click();
		const saveButton = page.getByRole("button", { name: "Save", exact: true });
		if (!(await saveButton.isVisible().catch(() => false))) {
			await page.getByRole("button", { name: "Back", exact: true }).click();
			continue;
		}
		const optionalHint = page.getByText(/API key is optional/i);
		if (await optionalHint.isVisible().catch(() => false)) {
			await page.getByRole("button", { name: "Back", exact: true }).click();
			continue;
		}
		return true;
	}
	return false;
}

test.describe("Provider setup page", () => {
	test("provider page loads", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await openProvidersPage(page);
		expect(pageErrors).toEqual([]);
	});

	test("add provider button exists", async ({ page }) => {
		await openProvidersPage(page);
		await expect(page.locator("#providersAddLlmBtn")).toBeVisible();
	});

	test("detect models button exists", async ({ page }) => {
		await openProvidersPage(page);
		await expect(page.locator("#providersDetectModelsBtn")).toBeVisible();
	});

	test("no providers shows guidance", async ({ page }) => {
		await openProvidersPage(page);

		// On a fresh server with no API keys, should show guidance or empty state
		const content = page.locator("#pageContent");
		await expect(content).not.toBeEmpty();
	});

	test("page has no JS errors", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await openProvidersPage(page);
		expect(pageErrors).toEqual([]);
	});

	test("provider modal honors configured provider order", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await openProvidersPage(page);
		await openProviderPicker(page);

		const providerNames = page.locator("#providerModalBody .provider-item .provider-item-name");
		await expect(providerNames.first()).toBeVisible();
		const names = await providerNames.allTextContents();
		const preferredOrder = ["OpenAI", "OpenRouter"];
		const expectedVisible = preferredOrder.filter((name) => names.includes(name));
		const actualVisible = names.filter((name) => expectedVisible.includes(name));
		expect(actualVisible).toEqual(expectedVisible);
		expect(pageErrors).toEqual([]);
	});

	test("OpenAI Compatible saves credentials for a config-declared custom provider", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const provider = {
			name: "custom-ai-example",
			displayName: "Example Compatible",
			configured: false,
			defaultBaseUrl: null,
			baseUrl: null,
			requiresModel: true,
			keyOptional: false,
			isCustom: true,
			uiOrder: 40,
		};

		await mockProviderModelContract(page, [], [provider]);
		await openProvidersPage(page);
		await openProviderPicker(page);

		const compatibleItem = page
			.locator("#providerModalBody .provider-item")
			.filter({ has: page.getByText("OpenAI Compatible", { exact: true }) })
			.first();
		await expect(compatibleItem).toBeVisible();
		await compatibleItem.click();
		await page.getByLabel("Configured provider", { exact: true }).selectOption("custom-ai-example");
		await page.getByRole("button", { name: "Continue", exact: true }).click();
		await page.locator("#providerModalBody input[type='password']").fill("sk-compatible");
		await page.getByLabel("Endpoint", { exact: true }).fill("https://ai.example.invalid/v1");
		await page.getByRole("button", { name: "Save", exact: true }).click();

		await expect
			.poll(() =>
				page.evaluate(() => window.__providerModelRequests.find((request) => request.method === "providers.save_key")),
			)
			.toBeTruthy();
		const saveRequest = await page.evaluate(() =>
			window.__providerModelRequests.find((request) => request.method === "providers.save_key"),
		);
		expect(saveRequest.params).toEqual({
			provider: "custom-ai-example",
			apiKey: "sk-compatible",
			baseUrl: "https://ai.example.invalid/v1",
		});
		expect(pageErrors).toEqual([]);
	});

	test("api key forms include provider key source hints", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await openProvidersPage(page);
		await openProviderPicker(page);

		if (await openApiKeyProviderForm(page)) {
			const sourceHint = page.locator("#providerModalBody a, #providerModalBody div").filter({
				hasText: /Get your key at|Get your API key from|API key is optional/i,
			});
			await expect(sourceHint.first()).toBeVisible();
		}

		expect(pageErrors).toEqual([]);
	});

	test("provider validation errors render in danger panel", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await openProvidersPage(page);
		await openProviderPicker(page);

		if (await openRequiredApiKeyProviderForm(page)) {
			await page.getByRole("button", { name: "Save", exact: true }).click();

			const errorPanel = page.locator("#providerModal .alert-error-text");
			await expect(errorPanel).toBeVisible();
			await expect(errorPanel).toContainText("Error:");
			await expect(errorPanel).toContainText("API key is required");
		}

		expect(pageErrors).toEqual([]);
	});

	test("save key backend refusal renders in the provider error panel", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const provider = {
			name: "openai",
			displayName: "OpenAI",
			configured: false,
			defaultBaseUrl: "https://api.openai.com/v1",
			baseUrl: null,
			requiresModel: false,
			keyOptional: false,
			isCustom: false,
			uiOrder: 30,
		};
		const errorMessage = "enabled provider `openai` has no configured models";

		await mockProviderModelContract(page, [], [provider], { saveKeyError: errorMessage });
		await openProvidersPage(page);
		await openProviderPicker(page);
		const openaiItem = page
			.locator("#providerModalBody .provider-item")
			.filter({ has: page.getByText("OpenAI", { exact: true }) })
			.first();
		await expect(openaiItem).toBeVisible();
		await openaiItem.click();
		await page.locator("#providerModalBody input[type='password']").fill("sk-test");
		await page.getByRole("button", { name: "Save", exact: true }).click();

		const errorPanel = page.locator("#providerModal .alert-error-text");
		await expect(errorPanel).toBeVisible();
		await expect(errorPanel).toContainText(errorMessage);
		await expect
			.poll(() =>
				page.evaluate(() => window.__providerModelRequests.find((request) => request.method === "providers.save_key")),
			)
			.toBeTruthy();
		const saveRequest = await page.evaluate(() =>
			window.__providerModelRequests.find((request) => request.method === "providers.save_key"),
		);
		expect(saveRequest.params).toEqual({ provider: "openai", apiKey: "sk-test" });
		expect(pageErrors).toEqual([]);
	});

	test("renders complete registry records and saves a canonical model ID subset", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const primaryModel = modelRecord({
			id: "openai::gpt-5",
			provider: "openai",
			preferred: true,
			disabled: false,
			unsupported: true,
			unsupportedReason: "Unavailable in this region",
			unsupportedProvider: "openai",
			unsupportedUpdatedAt: 1_735_689_700,
			contextLength: 256_000,
			maxInputTokens: 192_000,
			maxOutputTokens: 64_000,
			inputModalities: ["text", "image", "file"],
			outputModalities: ["text", "audio"],
			toolCalling: false,
			streaming: true,
			zeroDataRetentionEnabled: false,
			supportedEfforts: ["minimal", "medium", "xhigh"],
			reasoningSummary: "detailed",
			reasoningInclude: ["encrypted_content"],
		});
		const secondaryModel = modelRecord({
			id: "openai::o3-pro",
			provider: "openai",
			contextLength: 200_000,
			maxInputTokens: 160_000,
			maxOutputTokens: 40_000,
			inputModalities: ["text", "image"],
			outputModalities: ["text"],
			toolCalling: true,
			streaming: false,
			zeroDataRetentionEnabled: true,
			supportedEfforts: ["low", "high"],
			reasoningSummary: "concise",
			reasoningInclude: [],
		});
		const provider = {
			name: "openai",
			displayName: "OpenAI",
			configured: true,
			defaultBaseUrl: "https://api.openai.com/v1",
			baseUrl: null,
			requiresModel: false,
			keyOptional: false,
			isCustom: false,
			uiOrder: 30,
		};

		await mockProviderModelContract(page, [primaryModel, secondaryModel], [provider]);
		await openProvidersPage(page);
		await waitForWsConnected(page);

		const record = page.getByTestId("provider-model-record-openai::gpt-5");
		await expect(record).toBeVisible();
		const renderedFields = await record
			.locator(":scope > div")
			.evaluateAll((rows) =>
				Object.fromEntries(
					rows.map((row) => [
						(row.querySelector("dt")?.textContent || "").replace(/:$/, ""),
						row.querySelector("dd")?.textContent || "",
					]),
				),
			);
		expect(renderedFields).toEqual({
			id: "openai::gpt-5",
			provider: "openai",
			preferred: "true",
			disabled: "false",
			unsupported: "true",
			unsupported_reason: "Unavailable in this region",
			unsupported_provider: "openai",
			unsupported_updated_at: "1735689700",
			context_length: "256000",
			max_input_tokens: "192000",
			max_output_tokens: "64000",
			input_modalities: '["text","image","file"]',
			output_modalities: '["text","audio"]',
			tool_calling: "false",
			streaming: "true",
			zeroDataRetentionEnabled: "false",
			reasoning_supported_efforts: '["minimal","medium","xhigh"]',
			reasoning_summary: "detailed",
			reasoning_include: '["encrypted_content"]',
		});

		await page.locator("#provider-openai").getByRole("button", { name: "Preferred Models", exact: true }).click();
		await expect(page.locator("#providerModal")).toBeVisible();
		const cards = page.locator("#providerModalBody .model-card");
		await cards.filter({ hasText: "openai::o3-pro" }).click();
		await cards.filter({ hasText: "openai::gpt-5" }).click();
		await page.locator("#providerModalBody").getByRole("button", { name: "Save", exact: true }).click();

		await expect
			.poll(() =>
				page.evaluate(() =>
					window.__providerModelRequests.find((request) => request.method === "providers.set_model_preferences"),
				),
			)
			.toBeTruthy();
		const saveRequest = await page.evaluate(() =>
			window.__providerModelRequests.find((request) => request.method === "providers.set_model_preferences"),
		);

		expect(saveRequest.params).toEqual({
			provider: "openai",
			modelIds: ["openai::o3-pro"],
		});
		expect(pageErrors).toEqual([]);
	});
});
