const { expect, test } = require("../base-test");
const { navigateAndWait, watchPageErrors } = require("../helpers");

function terminal(overrides = {}) {
	return {
		kind: "execute",
		id: "42",
		sessionKey: "main",
		sessionId: "$4",
		sessionName: "chelix-main",
		windowId: "@8",
		windowName: "shell",
		paneId: "%11",
		running: true,
		...overrides,
	};
}

test.describe("Chat terminal modal", () => {
	test("opens beside MCP with compact active-session tabs and exact attachment", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const attached = terminal();
		const inventoryRequests = [];
		const websocketUrls = [];

		await page.route("**/api/terminal/terminals?**", async (route) => {
			inventoryRequests.push(route.request().url());
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ instanceId: "host", terminals: [attached] }),
			});
		});
		await page.routeWebSocket("/api/terminal/ws?**", (websocket) => {
			websocketUrls.push(websocket.url());
			websocket.onMessage((rawMessage) => {
				const message = JSON.parse(rawMessage.toString());
				if (message.type === "resize") {
					websocket.send(
						JSON.stringify({
							type: "output",
							encoding: "base64",
							data: Buffer.from("chat terminal output\r\n").toString("base64"),
						}),
					);
				}
			});
			websocket.send(JSON.stringify({ type: "ready", available: true, terminal: attached }));
		});

		await navigateAndWait(page, "/chats/main");
		const terminalButton = page.getByRole("button", { name: "Terminal", exact: true });
		await expect(terminalButton).toBeVisible();
		expect(
			await page.locator("#mcpToggleBtn").evaluate((button) => button.nextElementSibling?.id),
		).toBe("chatTerminalBtn");

		await terminalButton.click();
		await expect(page.locator("#chatTerminalModal")).toBeVisible();
		const tab = page.getByRole("button", { name: "Terminal 42, running", exact: true });
		await expect(tab).toBeVisible();
		await expect(tab).toHaveText("42");
		await expect(page.getByRole("button", { name: "New terminal tab", exact: true })).toBeVisible();
		await expect(page.getByLabel("Chat terminal output").locator(".xterm-rows")).toContainText(
			"chat terminal output",
		);

		const inventoryUrl = new URL(inventoryRequests[0]);
		expect(inventoryUrl.searchParams.get("sessionKey")).toBe("main");
		const websocketUrl = new URL(websocketUrls[0]);
		expect(websocketUrl.searchParams.get("instanceId")).toBe("host");
		expect(websocketUrl.searchParams.get("id")).toBe(attached.id);
		expect(websocketUrl.searchParams.get("sessionKey")).toBe("main");

		const spacing = await page.locator("#chatTerminalModal .chat-terminal-page").evaluate((element) => {
			const tabs = element.querySelector(".chat-terminal-tabs-bar");
			const output = element.querySelector(".chat-terminal-output-wrap");
			return {
				tabsPadding: tabs ? getComputedStyle(tabs).padding : "",
				outputPadding: output ? getComputedStyle(output).padding : "",
			};
		});
		expect(spacing.tabsPadding).toBe("3px 3px 0px");
		expect(spacing.outputPadding).toBe("2px");

		await page.getByRole("button", { name: "Close", exact: true }).click();
		await expect(page.locator("#chatTerminalModal")).toBeHidden();
		expect(pageErrors).toEqual([]);
	});

	test("keeps an empty terminal surface until the trailing plus creates a tab", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const created = terminal({
			id: "99",
			sessionId: "$9",
			windowId: "@12",
			paneId: "%15",
			running: false,
		});
		const terminals = [];
		const createBodies = [];

		await page.route("**/api/terminal/terminals?**", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ instanceId: "host", terminals }),
			});
		});
		await page.route("**/api/terminal/terminals", async (route) => {
			createBodies.push(route.request().postDataJSON());
			terminals.push(created);
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ instanceId: "host", terminal: created }),
			});
		});
		await page.routeWebSocket("/api/terminal/ws?**", (websocket) => {
			websocket.send(JSON.stringify({ type: "ready", available: true, terminal: created }));
		});

		await navigateAndWait(page, "/chats/main");
		await page.getByRole("button", { name: "Terminal", exact: true }).click();
		await expect(page.getByLabel("Chat terminal output")).toBeVisible();
		await expect(page.getByRole("button", { name: /^Terminal \d/ })).toHaveCount(0);

		await page.getByRole("button", { name: "New terminal tab", exact: true }).click();
		await expect(page.getByRole("button", { name: "Terminal 99, idle", exact: true })).toBeVisible();
		expect(createBodies).toEqual([{ sessionKey: "main" }]);
		expect(pageErrors).toEqual([]);
	});
});