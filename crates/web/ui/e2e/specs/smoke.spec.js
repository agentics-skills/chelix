const { expect, test } = require("../base-test");
const { expectPageContentMounted, watchPageErrors } = require("../helpers");

test("app shell loads chat route instead of onboarding", async ({ page }) => {
	const pageErrors = watchPageErrors(page);

	await page.goto("/");

	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);
	await expect(page.locator("#sessionsPanel")).toBeVisible();
	await expect(page.locator("#chatInput")).toBeVisible();
	await expect(page.locator("#statusDot")).toBeVisible();
	// statusDot should reach "connected" class; statusText is cleared to "" when connected
	await expect(page.locator("#statusDot")).toHaveClass(/connected/, { timeout: 15_000 });

	expect(pageErrors).toEqual([]);
});

test("chat shows exact read-only global sandbox status", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);

	const indicator = page.getByRole("status");
	await expect(indicator).toHaveText("Sandbox On");
	await expect(indicator).not.toHaveJSProperty("tagName", "BUTTON");
	await expect(page.locator("#sandboxToggle, #sandboxImageBtn, #sandboxImageDropdown")).toHaveCount(0);

	await page.evaluate(() => {
		const state = window.__chelix_state;
		const sandbox = window.__chelix_modules?.sandbox;
		const current = state?.sandboxInfo;
		if (!(state && sandbox && current)) throw new Error("sandbox E2E bridge unavailable");
		state.setSandboxInfo({ ...current, mode: "Off" });
		sandbox.updateSandboxUI();
	});
	await expect(indicator).toHaveText("Sandbox Off");
	await expect(indicator).not.toHaveAttribute("role", "button");

	await page.evaluate(() => {
		const state = window.__chelix_state;
		const sandbox = window.__chelix_modules?.sandbox;
		const current = state?.sandboxInfo;
		if (!(state && sandbox && current)) throw new Error("sandbox E2E bridge unavailable");
		state.setSandboxInfo({ ...current, mode: "On" });
		sandbox.updateSandboxUI();
	});
	await expect(indicator).toHaveText("Sandbox On");
	expect(pageErrors).toEqual([]);
});

test("desktop top bars stay compact and chat toolbar scrolls horizontally", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.setViewportSize({ width: 900, height: 720 });

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);

	await expect(page.locator("#sessionsPanel")).toBeVisible();
	await expect(page.locator("#settingsBtn")).toBeVisible();
	await expect(page.locator("#debugPanelBtn")).toBeVisible();

	const layout = await page.evaluate(() => {
		const header = document.querySelector("body > header");
		const toolbar = document.querySelector(".chat-toolbar");
		const buttons = Array.from(document.querySelectorAll(".chat-toolbar button"));
		return {
			headerHeight: header?.getBoundingClientRect().height || 0,
			toolbarHeight: toolbar?.getBoundingClientRect().height || 0,
			toolbarOverflowX: toolbar ? getComputedStyle(toolbar).overflowX : "",
			toolbarWrap: toolbar ? getComputedStyle(toolbar).flexWrap : "",
			maxToolbarButtonHeight: Math.max(0, ...buttons.map((button) => button.getBoundingClientRect().height)),
		};
	});

	expect(layout.headerHeight).toBeLessThanOrEqual(24);
	expect(layout.toolbarHeight).toBeLessThanOrEqual(24);
	expect(layout.maxToolbarButtonHeight).toBeLessThanOrEqual(20);
	expect(layout.toolbarOverflowX).toBe("auto");
	expect(layout.toolbarWrap).toBe("nowrap");
	expect(pageErrors).toEqual([]);
});

test("mobile chat toolbar exposes the same controls as desktop", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.setViewportSize({ width: 390, height: 844 });

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);

	for (const selector of [
		"#sandboxIndicator",
		"#mcpToggleBtn",
		"#debugPanelBtn",
		"#fullContextBtn",
		'button[title="Fork session"]',
		'button[title="Clear session"]',
	]) {
		await expect(page.locator(selector)).toBeVisible();
	}

	const layout = await page.evaluate(() => {
		const toolbar = document.querySelector(".chat-toolbar");
		return {
			toolbarOverflowX: toolbar ? getComputedStyle(toolbar).overflowX : "",
			toolbarWrap: toolbar ? getComputedStyle(toolbar).flexWrap : "",
		};
	});

	expect(layout.toolbarOverflowX).toBe("auto");
	expect(layout.toolbarWrap).toBe("nowrap");
	expect(pageErrors).toEqual([]);
});

test("index page exposes OG and Twitter share metadata", async ({ page }) => {
	const pageErrors = watchPageErrors(page);

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);

	await expect.poll(() => page.locator('meta[property="og:title"]').getAttribute("content")).toContain("AI assistant");
	await expect
		.poll(() => page.locator('meta[property="og:description"]').getAttribute("content"))
		.toContain("personal AI assistant");
	await expect(page.locator('meta[property="og:image"]')).toHaveAttribute(
		"content",
		"https://raw.githubusercontent.com/agentics-skills/chelix/master/crates/web/src/assets/icons/icon-512.png",
	);
	await expect(page.locator('meta[name="twitter:card"]')).toHaveAttribute("content", "summary_large_image");
	await expect(page.locator('meta[name="twitter:image"]')).toHaveAttribute(
		"content",
		"https://raw.githubusercontent.com/agentics-skills/chelix/master/crates/web/src/assets/icons/icon-512.png",
	);

	expect(pageErrors).toEqual([]);
});

test("mobile menu drives settings and sessions", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.setViewportSize({ width: 390, height: 844 });

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);

	await expect(page.locator("#settingsBtn")).toBeHidden();
	await expect(page.locator("#mobileMenuBtn")).toBeVisible();
	await page.locator("#mobileMenuBtn").click();
	await expect(page.locator("#mobileMenuPanel")).toHaveClass(/open/);
	await page.locator("#mobileMenuSettingsBtn").click();
	await expect(page).toHaveURL(/\/settings\/profile$/);
	await expect(page.locator(".settings-sidebar")).toHaveCount(0);
	await page.locator(".settings-mobile-menu-btn").click();
	await expect(page.locator(".settings-sidebar")).toBeVisible();
	await page.locator(".settings-nav-item", { hasText: "Memory" }).click();
	await expect(page).toHaveURL(/\/settings\/memory$/);
	await expect(page.locator(".settings-sidebar")).toHaveCount(0);
	await expect(page.getByText("Memory Style", { exact: true })).toBeVisible();
	await expect(page.getByText("Prompt Memory Mode", { exact: true })).toBeVisible();
	await expect(page.getByText("Agent Memory Writes", { exact: true })).toBeVisible();
	await expect(page.getByText("USER.md Writes", { exact: true })).toBeVisible();
	await expect(page.getByText("Embedding Provider", { exact: true })).toBeVisible();
	await expect(page.getByText("Search Merge Strategy", { exact: true })).toBeVisible();
	await expect(page.getByText("Session Export", { exact: true })).toBeVisible();
	await page.locator(".settings-mobile-menu-btn").click();
	var voiceNav = page.locator(".settings-nav-item", { hasText: "Voice" });
	await voiceNav.scrollIntoViewIfNeeded();
	await voiceNav.click();
	await expect(page).toHaveURL(/\/settings\/voice$/);
	await expect(page.locator(".settings-sidebar")).toHaveCount(0);
	await page.locator(".settings-mobile-menu-btn").click();
	var heartbeatNav = page.locator(".settings-nav-item", { hasText: "Heartbeat" });
	await heartbeatNav.scrollIntoViewIfNeeded();
	await heartbeatNav.click();
	await expect(page).toHaveURL(/\/settings\/heartbeat$/);
	await expect(
		page.getByRole("heading", {
			name: "Heartbeat",
			exact: true,
		}),
	).toBeVisible();

	await page.goto("/chats/main");
	await expectPageContentMounted(page);
	await expect(page.locator("#sessionsToggle")).toBeHidden();
	await page.locator("#mobileMenuBtn").click();
	await page.locator("#mobileMenuSessionsBtn").click();
	await expect(page.locator("#sessionsPanel")).toHaveClass(/open/);
	await expect(page.locator("#sessionsOverlay")).toHaveClass(/visible/);

	expect(pageErrors).toEqual([]);
});

test("standalone mobile layout avoids double safe-area padding", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.setViewportSize({ width: 390, height: 844 });
	await page.addInitScript(() => {
		const originalMatchMedia = window.matchMedia.bind(window);
		window.matchMedia = (query) => {
			if (query === "(display-mode: standalone)") {
				return {
					matches: true,
					media: query,
					onchange: null,
					addListener() {
						return undefined;
					},
					removeListener() {
						return undefined;
					},
					addEventListener() {
						return undefined;
					},
					removeEventListener() {
						return undefined;
					},
					dispatchEvent() {
						return true;
					},
				};
			}
			return originalMatchMedia(query);
		};
		Object.defineProperty(navigator, "standalone", {
			configurable: true,
			get() {
				return true;
			},
		});
	});

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);
	await expect(page.locator("html")).toHaveClass(/pwa-standalone/);

	await page.evaluate(() => {
		document.documentElement.style.setProperty("--safe-top", "24px");
		document.documentElement.style.setProperty("--safe-bottom", "18px");
	});

	const layout = await page.evaluate(() => {
		const body = getComputedStyle(document.body);
		const header = getComputedStyle(document.querySelector("header"));
		const inputRow = getComputedStyle(document.querySelector(".chat-input-row"));
		return {
			bodyPaddingTop: parseFloat(body.paddingTop),
			headerPaddingTop: parseFloat(header.paddingTop),
			inputPaddingBottom: parseFloat(inputRow.paddingBottom),
		};
	});

	expect(layout.bodyPaddingTop).toBe(0);
	expect(layout.headerPaddingTop).toBeGreaterThan(24);
	expect(layout.inputPaddingBottom).toBeGreaterThan(18);
	expect(pageErrors).toEqual([]);
});

test("standalone mobile viewport uses screen height when initial viewport is short", async ({ page }) => {
	const pageErrors = watchPageErrors(page);
	await page.setViewportSize({ width: 390, height: 760 });
	await page.addInitScript(() => {
		const originalMatchMedia = window.matchMedia.bind(window);
		window.matchMedia = (query) => {
			if (query === "(display-mode: standalone)") {
				return {
					matches: true,
					media: query,
					onchange: null,
					addListener() {
						return undefined;
					},
					removeListener() {
						return undefined;
					},
					addEventListener() {
						return undefined;
					},
					removeEventListener() {
						return undefined;
					},
					dispatchEvent() {
						return true;
					},
				};
			}
			return originalMatchMedia(query);
		};
		Object.defineProperty(navigator, "standalone", {
			configurable: true,
			get() {
				return true;
			},
		});
		Object.defineProperty(window.screen, "width", {
			configurable: true,
			get() {
				return 390;
			},
		});
		Object.defineProperty(window.screen, "height", {
			configurable: true,
			get() {
				return 844;
			},
		});
	});

	await page.goto("/");
	await expect(page).toHaveURL(/\/chats\/main$/);
	await expectPageContentMounted(page);

	const layout = await page.evaluate(() => ({
		heightVar: getComputedStyle(document.documentElement).getPropertyValue("--mobile-viewport-height").trim(),
		bodyHeight: document.body.getBoundingClientRect().height,
		inputBottom: document.querySelector(".chat-input-row")?.getBoundingClientRect().bottom || 0,
	}));

	expect(layout.heightVar).toBe("844px");
	expect(layout.bodyHeight).toBeCloseTo(844, 0);
	expect(layout.inputBottom).toBeCloseTo(844, 0);
	expect(pageErrors).toEqual([]);
});

const routeCases = [
	{
		path: "/settings/crons",
		expectedUrl: /\/settings\/crons$/,
		heading: "Cron Jobs",
	},
	{
		path: "/monitoring",
		expectedUrl: /\/monitoring$/,
		heading: "Monitoring",
	},
	{
		path: "/skills",
		expectedUrl: /\/skills$/,
		heading: "Skills",
	},
	{
		path: "/projects",
		expectedUrl: /\/projects$/,
		heading: "Repositories",
	},
	{
		path: "/settings",
		expectedUrl: /\/settings\/profile$/,
		settingsActive: true,
		heading: "User Profile",
	},
];

for (const routeCase of routeCases) {
	test(`route ${routeCase.path} renders without uncaught errors`, async ({ page }) => {
		const pageErrors = watchPageErrors(page);

		await page.goto(routeCase.path);

		await expect(page).toHaveURL(routeCase.expectedUrl);
		await expectPageContentMounted(page);
		if (routeCase.settingsActive) {
			await expect(page.locator("#settingsBtn")).toHaveClass(/active/);
		}
		await expect(
			page.getByRole("heading", {
				name: routeCase.heading,
				exact: true,
			}),
		).toBeVisible();

		expect(pageErrors).toEqual([]);
	});
}
