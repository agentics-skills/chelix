const { expect, test } = require("../base-test");
const { navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

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
		const preferredOrder = ["GitHub Copilot", "Anthropic", "OpenAI", "Ollama"];
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
});
