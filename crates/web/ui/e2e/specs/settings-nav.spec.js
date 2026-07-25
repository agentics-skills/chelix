const { expect, test } = require("../base-test");
const { navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

function graphqlHttpStatus(page) {
	return page.evaluate(async () => {
		const response = await fetch("/graphql", {
			method: "GET",
			redirect: "manual",
		});
		return response.status;
	});
}

test.describe("Settings navigation", () => {
	async function openProvidersPage(page) {
		await navigateAndWait(page, "/settings/providers");
		await expect.poll(() => new URL(page.url()).pathname).toBe("/settings/providers");
		await expect(page.locator("#providersTitle")).toBeVisible();
	}

	test("/settings redirects to /settings/profile", async ({ page }) => {
		await navigateAndWait(page, "/settings");
		await expect(page).toHaveURL(/\/settings\/profile$/);
		await expect(page.getByRole("heading", { name: "User Profile", exact: true })).toBeVisible();
	});

	test("settings nav keeps distinct icons for remote access, tools, and mcp", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/profile");
		await expect(page.locator(".settings-sidebar-nav")).toBeVisible();

		const masks = await page.evaluate(() => {
			const readableRules = (sheet) => {
				try {
					return Array.from(sheet.cssRules || []);
				} catch {
					return [];
				}
			};

			const readRuleMask = (selector) => {
				for (const sheet of Array.from(document.styleSheets || [])) {
					const rule = readableRules(sheet).find(
						(candidate) => candidate.type === CSSRule.STYLE_RULE && candidate.selectorText === selector,
					);
					if (rule)
						return rule.style.getPropertyValue("-webkit-mask-image") || rule.style.getPropertyValue("mask-image") || "";
				}
				return null;
			};
			return {
				ssh: readRuleMask('.settings-nav-item[data-section="ssh"]::before'),
				tools: readRuleMask('.settings-nav-item[data-section="tools"]::before'),
				mcp: readRuleMask('.settings-nav-item[data-section="mcp"]::before'),
			};
		});

		const hasMask = (value) => {
			if (typeof value !== "string") return false;
			const normalized = value.trim().toLowerCase();
			return normalized !== "" && normalized !== "none";
		};
		if (masks.ssh !== null) {
			expect(hasMask(masks.ssh)).toBeTruthy();
		}
		expect(hasMask(masks.tools)).toBeTruthy();
		expect(hasMask(masks.mcp)).toBeTruthy();

		expect(pageErrors).toEqual([]);
	});

	const settingsSections = [
		{ id: "profile", heading: "User Profile" },
		{ id: "memory", heading: "Memory" },
		{ id: "environment", heading: "Environment" },
		{ id: "crons", heading: "Cron Jobs" },
		{ id: "voice", heading: "Voice" },
		{ id: "phone", heading: "Phone" },
		{ id: "security", heading: "Security" },
		{ id: "ssh", heading: "SSH" },
		{ id: "notifications", heading: "Notifications" },
		{ id: "providers", heading: "LLMs" },
		{ id: "tools", heading: "Tools" },
		{ id: "channels", heading: "Channels" },
		{ id: "mcp", heading: "MCP" },
		{ id: "hooks", heading: "Hooks" },
		{ id: "skills", heading: "Skills" },
		{ id: "projects", heading: "Repositories" },
		{ id: "sandboxes", heading: "Sandboxes" },
		{ id: "monitoring", heading: "Monitoring" },
		{ id: "logs", heading: "Logs" },
		{ id: "config", heading: "Configuration" },
	];

	for (const section of settingsSections) {
		test(`settings/${section.id} loads without errors`, async ({ page }) => {
			const pageErrors = watchPageErrors(page);
			await navigateAndWait(page, `/settings/${section.id}`);
			await waitForWsConnected(page);

			await expect(page).toHaveURL(new RegExp(`/settings/${section.id}$`));

			// Settings sections use heading text that may differ slightly
			// from the section ID; check the page loaded content.
			const content = page.locator("#pageContent");
			await expect(content).not.toBeEmpty();

			expect(pageErrors).toEqual([]);
		});
	}

	test("voice settings saves whisper base URL without requiring an API key", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/voice");
		await waitForWsConnected(page);

		const whisperRow = page
			.locator(".provider-card")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: true }) })
			.first();
		await expect(whisperRow).toBeVisible();

		await whisperRow.getByRole("button", { name: "Configure", exact: true }).click();
		let modal = page
			.locator(".modal-box")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: false }) })
			.last();
		await expect(modal).toBeVisible();
		await modal.locator('input[data-field="baseUrl"]').fill("http://127.0.0.1:8001/v1");
		await modal.getByRole("button", { name: "Save", exact: true }).click();

		await expect(modal).toBeHidden();

		await whisperRow.getByRole("button", { name: "Configure", exact: true }).click();
		modal = page
			.locator(".modal-box")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: false }) })
			.last();
		await expect(modal).toBeVisible();
		await expect(modal.locator('input[data-field="baseUrl"]')).toHaveValue("http://127.0.0.1:8001/v1");
		expect(pageErrors).toEqual([]);
	});

	test("voice settings can clear an existing whisper base URL", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/voice");
		await waitForWsConnected(page);

		const whisperRow = page
			.locator(".provider-card")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: true }) })
			.first();
		await expect(whisperRow).toBeVisible();

		await whisperRow.getByRole("button", { name: "Configure", exact: true }).click();
		let modal = page
			.locator(".modal-box")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: false }) })
			.last();
		await expect(modal).toBeVisible();
		await modal.locator('input[data-field="baseUrl"]').fill("http://127.0.0.1:8001/v1");
		await modal.getByRole("button", { name: "Save", exact: true }).click();

		await expect(modal).toBeHidden();

		await whisperRow.getByRole("button", { name: "Configure", exact: true }).click();
		modal = page
			.locator(".modal-box")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: false }) })
			.last();
		await expect(modal).toBeVisible();
		await expect(modal.locator('input[data-field="baseUrl"]')).toHaveValue("http://127.0.0.1:8001/v1");
		await modal.locator('input[data-field="baseUrl"]').fill("");
		await modal.getByRole("button", { name: "Save", exact: true }).click();

		await expect(modal).toBeHidden();

		await whisperRow.getByRole("button", { name: "Configure", exact: true }).click();
		modal = page
			.locator(".modal-box")
			.filter({ has: page.getByText("OpenAI Whisper", { exact: false }) })
			.last();
		await expect(modal).toBeVisible();
		await expect(modal.locator('input[data-field="baseUrl"]')).toHaveValue("");
		expect(pageErrors).toEqual([]);
	});

	test("identity form elements render", async ({ page }) => {
		await navigateAndWait(page, "/settings/profile");

		// Identity page should have a name input and soul/description textarea
		const content = page.locator("#pageContent");
		await expect(content).not.toBeEmpty();
	});

	test("tools settings shows effective inventory and routing summary", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/tools");

		await expect(page.getByRole("heading", { name: "Tools", exact: true })).toBeVisible();
		await expect(
			page.getByText("This page shows the effective tool inventory for the active session and model.", {
				exact: false,
			}),
		).toBeVisible();
		await expect(page.getByText("Tool Calling", { exact: true })).toBeVisible();
		await expect(page.getByText("Execution Runtime", { exact: true })).toBeVisible();
		await expect(page.getByText("Registered Tools", { exact: true })).toBeVisible();

		expect(pageErrors).toEqual([]);
	});

	test("identity name fields autosave on blur", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/profile");

		const userNameInput = page.getByPlaceholder("e.g. Alice");
		await expect(userNameInput).toBeVisible({ timeout: 5_000 });
		const currentVal = await userNameInput.inputValue();
		const nextUserName = currentVal === "AutoUserNameA" ? "AutoUserNameB" : "AutoUserNameA";

		await userNameInput.fill(nextUserName);
		await expect(userNameInput).toHaveValue(nextUserName);
		await userNameInput.blur();

		// Wait for the "Saved" flash (confirms autosave round-tripped).
		await expect(page.getByText("Saved")).toBeVisible({ timeout: 15_000 });

		// Reload and verify the value persisted.
		await page.reload();
		await expect(page.getByPlaceholder("e.g. Alice")).toHaveValue(nextUserName, { timeout: 10_000 });

		expect(pageErrors).toEqual([]);
	});

	// The profile page no longer owns favicon-related emoji controls.
	// Those checks are intentionally covered elsewhere in the agents UI.

	test("environment page has add form", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/environment");
		await expect(page.getByRole("heading", { name: "Environment" })).toBeVisible();
		const addForm = page.getByRole("form", { name: "Add environment variable" });
		await expect(addForm.getByPlaceholder("KEY_NAME")).toHaveAttribute("autocomplete", "off");
		await expect(addForm.getByPlaceholder("Value")).toHaveAttribute("autocomplete", "new-password");
		await expect(addForm.getByRole("checkbox", { name: /^Secret/ })).toBeChecked();
		await expect(addForm.getByRole("checkbox", { name: /^Enabled/ })).toBeChecked();
		expect(pageErrors).toEqual([]);
	});

	test("environment controls preserve rows and expose only non-secret values", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const timestamp = "2026-07-16T12:00:00Z";
		const variables = [
			{
				id: 101,
				key: "VISIBLE_SETTING",
				rawValue: "visible-value",
				secret: false,
				enabled: true,
				encrypted: true,
			},
			{
				id: 102,
				key: "SECRET_SETTING",
				rawValue: "top-secret-value",
				secret: true,
				enabled: true,
				encrypted: true,
			},
		];
		const posts = [];
		const patches = [];
		const listBody = () => ({
			env_vars: variables.map(({ rawValue, ...variable }) => ({
				...variable,
				value: variable.secret ? null : rawValue,
				created_at: timestamp,
				updated_at: timestamp,
			})),
		});
		const fulfillJson = (route, body) =>
			route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(body) });

		await page.route("**/api/env", async (route) => {
			const request = route.request();
			if (request.method() === "GET") {
				await fulfillJson(route, listBody());
				return;
			}
			if (request.method() !== "POST") throw new Error(`Unexpected /api/env method: ${request.method()}`);

			const body = request.postDataJSON();
			posts.push(body);
			variables.push({
				id: 102 + posts.length,
				key: body.key,
				rawValue: body.value,
				secret: body.secret,
				enabled: body.enabled,
				encrypted: true,
			});
			await fulfillJson(route, { ok: true });
		});

		await page.route("**/api/env/*", async (route) => {
			const request = route.request();
			if (request.method() !== "PATCH") throw new Error(`Unexpected env item method: ${request.method()}`);
			const id = Number(new URL(request.url()).pathname.split("/").pop());
			const body = request.postDataJSON();
			patches.push({ id, body });
			const variable = variables.find((candidate) => candidate.id === id);
			if (!variable) throw new Error(`Unknown mocked environment variable: ${id}`);
			variable.secret = body.secret;
			variable.enabled = body.enabled;
			await fulfillJson(route, { ok: true });
		});

		await navigateAndWait(page, "/settings/environment");
		await expect(page.getByRole("heading", { name: "Environment" })).toBeVisible();

		const rowFor = (key) => page.locator(".provider-item").filter({ has: page.getByText(key, { exact: true }) });
		const visibleRow = rowFor("VISIBLE_SETTING");
		const secretRow = rowFor("SECRET_SETTING");
		await expect(visibleRow.getByText("visible-value", { exact: true })).toBeVisible();
		await expect(secretRow.getByText("••••••••", { exact: true })).toBeVisible();
		await expect(page.getByText("top-secret-value", { exact: true })).toHaveCount(0);

		await visibleRow.getByRole("checkbox", { name: "Enabled", exact: true }).uncheck();
		await expect(visibleRow).toBeVisible();
		await expect(visibleRow.getByRole("checkbox", { name: "Enabled", exact: true })).not.toBeChecked();
		await expect.poll(() => patches.length).toBe(1);
		expect(patches[0]).toEqual({ id: 101, body: { secret: false, enabled: false } });
		expect(patches[0].body).not.toHaveProperty("value");

		await secretRow.getByRole("checkbox", { name: "Secret", exact: true }).uncheck();
		await expect(secretRow.getByText("top-secret-value", { exact: true })).toBeVisible();
		await expect.poll(() => patches.length).toBe(2);
		expect(patches[1]).toEqual({ id: 102, body: { secret: false, enabled: true } });

		const addForm = page.getByRole("form", { name: "Add environment variable" });
		await addForm.getByPlaceholder("KEY_NAME").fill("NEW_DEFAULT_SETTING");
		await addForm.getByPlaceholder("Value").fill("new-secret-value");
		await addForm.getByRole("button", { name: "Add", exact: true }).click();
		await expect(rowFor("NEW_DEFAULT_SETTING")).toBeVisible();
		expect(posts).toEqual([{ key: "NEW_DEFAULT_SETTING", value: "new-secret-value", secret: true, enabled: true }]);
		await expect(addForm.getByRole("checkbox", { name: /^Secret/ })).toBeChecked();
		await expect(addForm.getByRole("checkbox", { name: /^Enabled/ })).toBeChecked();

		await addForm.getByRole("checkbox", { name: /^Secret/ }).uncheck();
		await expect(addForm.getByPlaceholder("Value")).toHaveAttribute("type", "text");
		await addForm.getByPlaceholder("KEY_NAME").fill("NEW_VISIBLE_SETTING");
		await addForm.getByPlaceholder("Value").fill("new-visible-value");
		await addForm.getByRole("button", { name: "Add", exact: true }).click();
		const newVisibleRow = rowFor("NEW_VISIBLE_SETTING");
		await expect(newVisibleRow.getByText("new-visible-value", { exact: true })).toBeVisible();
		expect(posts[1]).toEqual({
			key: "NEW_VISIBLE_SETTING",
			value: "new-visible-value",
			secret: false,
			enabled: true,
		});
		await expect(addForm.getByRole("checkbox", { name: /^Secret/ })).toBeChecked();
		await expect(addForm.getByRole("checkbox", { name: /^Enabled/ })).toBeChecked();
		expect(pageErrors).toEqual([]);
	});

	test("security page renders", async ({ page }) => {
		await navigateAndWait(page, "/settings/security");
		await expect(page.getByRole("heading", { name: "Authentication" })).toBeVisible();
	});

	test("encryption page shows vault status when vault is enabled", async ({ page }) => {
		await navigateAndWait(page, "/settings/vault");
		const heading = page.getByRole("heading", { name: "Encryption" });
		const hasVault = await heading.isVisible().catch(() => false);
		if (hasVault) {
			await expect(heading).toBeVisible();
			// Should show a status badge
			const badges = page.locator(".provider-item-badge");
			await expect(badges.first()).toBeVisible();
		}
	});

	test("environment page shows encrypted badges on env vars", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/environment");
		await expect(page.getByRole("heading", { name: "Environment" })).toBeVisible();
		// If env vars exist, each row should show storage state independently of visibility.
		const items = page.locator(".provider-item");
		const count = await items.count();
		if (count > 0) {
			const firstItem = items.first();
			const hasBadge = await firstItem.locator(".provider-item-badge").count();
			expect(hasBadge).toBeGreaterThan(0);
			const badgeText = await firstItem.locator(".provider-item-badge").allTextContents();
			expect(badgeText.some((text) => ["Encrypted", "Plaintext"].includes(text.trim()))).toBeTruthy();
		}
		expect(pageErrors).toEqual([]);
	});

	test("provider page renders from settings", async ({ page }) => {
		await openProvidersPage(page);
	});

	test("terminal page renders from settings", async ({ page }) => {
		await page.route("**/api/terminal/instances", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ instances: [] }),
			});
		});
		await navigateAndWait(page, "/settings/terminal");
		await expect(page.getByRole("heading", { name: "Terminal", exact: true })).toBeVisible();
		await expect(page.getByLabel("Managed terminal output").locator(".xterm")).toHaveCount(1);
		await expect(page.getByLabel("Tools service instance")).toBeDisabled();
		await expect(page.getByLabel("Session key for a new terminal")).toBeVisible();
		await expect(page.getByText("No active tools service instances are registered.", { exact: true })).toBeVisible();
	});

	test("graphql toggle applies immediately", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/settings/profile");
		await waitForWsConnected(page);

		const graphQlNavItem = page.locator(".settings-nav-item", { hasText: "GraphQL" });
		const hasGraphql = (await graphQlNavItem.count()) > 0;
		test.skip(!hasGraphql, "GraphQL feature not enabled in this build");

		await graphQlNavItem.click();
		await expect(page).toHaveURL(/\/settings\/graphql$/);

		const toggleSwitch = page.locator("#graphqlToggleSwitch");
		const toggle = page.locator("#graphqlEnabledToggle");
		await expect(toggleSwitch).toBeVisible();
		const initial = await toggle.isChecked();
		const settingsUrl = new URL(page.url());
		const httpEndpoint = `${settingsUrl.origin}/graphql`;
		const wsScheme = settingsUrl.protocol === "https:" ? "wss:" : "ws:";
		const wsEndpoint = `${wsScheme}//${settingsUrl.host}/graphql`;

		await toggleSwitch.click();
		await expect.poll(() => toggle.isChecked()).toBe(!initial);

		await expect.poll(async () => graphqlHttpStatus(page)).toBe(initial ? 503 : 200);
		if (initial) {
			await expect(page.locator('iframe[title="GraphiQL Playground"]')).toHaveCount(0);
		} else {
			await expect(page.getByText(httpEndpoint, { exact: true })).toBeVisible();
			await expect(page.getByText(wsEndpoint, { exact: true })).toBeVisible();
			await expect(page.locator('iframe[title="GraphiQL Playground"]')).toBeVisible();
		}

		await toggleSwitch.click();
		await expect.poll(() => toggle.isChecked()).toBe(initial);
		await expect.poll(async () => graphqlHttpStatus(page)).toBe(initial ? 200 : 503);
		if (initial) {
			await expect(page.getByText(httpEndpoint, { exact: true })).toBeVisible();
			await expect(page.getByText(wsEndpoint, { exact: true })).toBeVisible();
			await expect(page.locator('iframe[title="GraphiQL Playground"]')).toBeVisible();
		}

		expect(pageErrors).toEqual([]);
	});

	test("sidebar groups and order match product layout", async ({ page }) => {
		await navigateAndWait(page, "/settings/profile");

		await expect(page.locator(".settings-group-label").nth(0)).toHaveText("General");
		await expect(page.locator(".settings-group-label").nth(1)).toHaveText("Security");
		await expect(page.locator(".settings-group-label").nth(2)).toHaveText("Integrations");
		await expect(page.locator(".settings-group-label").nth(3)).toHaveText("Systems");

		const navItems = (await page.locator(".settings-nav-item").allTextContents()).map((text) => text.trim());
		const presentOptionalItems = (items) => items.filter((item) => navItems.includes(item));
		const expected = [
			"User Profile",
			"Agents",
			"Projects",
			"Environment",
			"Memory",
			"Notifications",
			"Crons",
			"Webhooks",
			"Heartbeat",
			"Authentication",
			...presentOptionalItems(["Encryption", "SSH"]),
			"Network Audit",
			"Sandboxes",
			"Channels",
			"Hooks",
			"LLMs",
			"Tools",
			"MCP",
			"Skills",
			...presentOptionalItems(["Imports"]),
			...presentOptionalItems(["Voice", "Phone"]),
			"Terminal",
			"Monitoring",
			"Logs",
			...presentOptionalItems(["GraphQL"]),
			"Configuration",
		];
		expect(navItems).toEqual(expected);

		await expect(page.locator('.settings-nav-item[data-section="providers"]')).toHaveText("LLMs");
		await expect(page.locator('.settings-nav-item[data-section="logs"]')).toHaveText("Logs");
		await expect(page.locator('.settings-nav-item[data-section="terminal"]')).toHaveText("Terminal");
		await expect(page.locator('.settings-nav-item[data-section="config"]')).toHaveText("Configuration");

		if (navItems.includes("GraphQL")) {
			await expect(page.locator('.settings-nav-item[data-section="graphql"]')).toHaveText("GraphQL");
		}
	});
});
