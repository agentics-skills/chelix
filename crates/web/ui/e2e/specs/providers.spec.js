const { expect, test } = require("../base-test");
const { modelRecord, navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

function modelMetadata(model) {
	return {
		context_length: model.context_length,
		max_input_tokens: model.max_input_tokens,
		max_output_tokens: model.max_output_tokens,
		input_modalities: model.input_modalities,
		output_modalities: model.output_modalities,
		tool_calling: model.tool_calling,
		streaming: model.streaming,
		zeroDataRetentionEnabled: model.zeroDataRetentionEnabled,
		reasoning: model.reasoning,
	};
}

async function mockProviderModelContract(page, models, providers) {
	await page.addInitScript(
		({ models, providers }) => {
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

			WebSocket.prototype.send = function (data) {
				try {
					const request = JSON.parse(data);
					if (request?.method === "models.list" || request?.method === "models.list_all") {
						respond(this, request.id, models);
						return;
					}
					if (request?.method === "providers.available") {
						respond(this, request.id, providers);
						return;
					}
					if (request?.method === "models.test" || request?.method === "providers.save_models") {
						window.__providerModelRequests.push({ method: request.method, params: request.params || {} });
						respond(this, request.id, {});
						return;
					}
				} catch {
					// Fall through to the real WebSocket for unrelated requests.
				}
				return originalSend.call(this, data);
			};
		},
		{ models, providers },
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
		const preferredOrder = ["GitHub Copilot", "Anthropic", "OpenAI"];
		const expectedVisible = preferredOrder.filter((name) => names.includes(name));
		const actualVisible = names.filter((name) => expectedVisible.includes(name));
		expect(actualVisible).toEqual(expectedVisible);
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

	test("renders complete registry records and saves an ordered metadata map", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const primaryModel = modelRecord({
			id: "openai::gpt-5",
			provider: "openai",
			displayName: "GPT-5 Registry Record",
			createdAt: 1_735_689_600,
			recommended: true,
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
			reasoningInclude: ["reasoning.encrypted_content"],
		});
		const secondaryModel = modelRecord({
			id: "openai::o3-pro",
			provider: "openai",
			displayName: "O3 Pro Registry Record",
			createdAt: 1_735_689_800,
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
			authType: "api-key",
			configured: true,
			defaultBaseUrl: "https://api.openai.com/v1",
			baseUrl: null,
			models: {},
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
		const renderedFields = await record.locator(":scope > div").evaluateAll((rows) =>
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
			display_name: "GPT-5 Registry Record",
			created_at: "1735689600",
			recommended: "true",
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
			"reasoning.supported_efforts": '["minimal","medium","xhigh"]',
			"reasoning.summary": "detailed",
			"reasoning.include": '["reasoning.encrypted_content"]',
		});

		await page
			.locator("#provider-openai")
			.getByRole("button", { name: "Preferred Models", exact: true })
			.click();
		await expect(page.locator("#providerModal")).toBeVisible();
		const cards = page.locator("#providerModalBody .model-card");
		await cards.filter({ hasText: "O3 Pro Registry Record" }).click();
		await cards.filter({ hasText: "GPT-5 Registry Record" }).click();
		await page.locator("#providerModalBody").getByRole("button", { name: "Save", exact: true }).click();

		await expect
			.poll(() =>
				page.evaluate(() =>
					window.__providerModelRequests.find((request) => request.method === "providers.save_models"),
				),
			)
			.toBeTruthy();
		const saveRequest = await page.evaluate(() =>
			window.__providerModelRequests.find((request) => request.method === "providers.save_models"),
		);
		expect(saveRequest.params.provider).toBe("openai");
		expect(Array.isArray(saveRequest.params.models)).toBe(false);
		expect(Object.keys(saveRequest.params.models)).toEqual(["o3-pro", "gpt-5"]);
		expect(saveRequest.params.models).toEqual({
			"o3-pro": modelMetadata(secondaryModel),
			"gpt-5": modelMetadata(primaryModel),
		});
		expect(pageErrors).toEqual([]);
	});
});
