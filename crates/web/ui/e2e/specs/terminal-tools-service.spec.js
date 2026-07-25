const { expect, test } = require("../base-test");
const { navigateAndWait, watchPageErrors } = require("../helpers");

function terminal(overrides = {}) {
	return {
		id: "terminal-agent-42",
		sessionKey: "agent:session:42",
		sessionId: "$4",
		sessionName: "chelix-agent-session-42",
		windowId: "@8",
		windowName: "agent-shell",
		paneId: "%11",
		running: true,
		...overrides,
	};
}

test.describe("Tools service terminals", () => {
	test("uses exact service terminal IDs for inventory, creation, attachment, and control", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const instanceId = "docker:chelix-sandbox-agent-42";
		const terminals = [
			terminal(),
			terminal({
				id: "terminal-other-session-7",
				sessionKey: "agent:session:other",
				sessionId: "$5",
				windowId: "@9",
				paneId: "%12",
			}),
		];
		const clientMessages = [];
		const websocketUrls = [];
		const createRequests = [];
		let outputSent = false;

		await page.route("**/api/terminal/instances", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({
					instances: [
						{
							id: instanceId,
							label: "chelix-sandbox-agent-42 (docker)",
							terminals,
						},
					],
				}),
			});
		});
		await page.route("**/api/terminal/instances/*/terminals", async (route) => {
			const body = route.request().postDataJSON();
			createRequests.push({ url: route.request().url(), body });
			const created = terminal({
				id: "terminal-ui-created-99",
				sessionKey: body.sessionKey,
				sessionId: "$9",
				windowId: "@12",
				paneId: "%15",
				running: false,
			});
			terminals.push(created);
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ terminal: created }),
			});
		});
		await page.routeWebSocket("/api/terminal/ws?**", (websocket) => {
			websocketUrls.push(websocket.url());
			const terminalId = new URL(websocket.url()).searchParams.get("id");
			const attachedTerminal = terminals.find((candidate) => candidate.id === terminalId);
			websocket.onMessage((rawMessage) => {
				const message = JSON.parse(rawMessage.toString());
				clientMessages.push(message);
				if (message.type === "resize" && !outputSent) {
					outputSent = true;
					websocket.send(
						JSON.stringify({
							type: "output",
							encoding: "base64",
							data: Buffer.from("exact tools service output\r\n").toString("base64"),
						}),
					);
				}
			});
			websocket.send(
				JSON.stringify({
					type: "ready",
					available: true,
					mode: "tools_service",
					terminal: attachedTerminal,
				}),
			);
		});

		await navigateAndWait(page, "/settings/terminal");
		await expect(page.getByRole("heading", { name: "Terminal", exact: true })).toBeVisible();
		await expect(page.getByLabel("Agent session").locator("option:checked")).toHaveText("agent:session:42");
		await expect(page.getByRole("button", { name: /terminal-agent-42/ })).toBeVisible();
		await expect(page.getByRole("button", { name: /terminal-other-session-7/ })).toHaveCount(0);

		await page.getByRole("button", { name: /terminal-agent-42/ }).click();
		await expect(page.getByText("terminal: terminal-agent-42", { exact: true })).toBeVisible();
		await expect(page.getByText("session: $4", { exact: true })).toBeVisible();
		await expect(page.getByText("window: @8", { exact: true })).toBeVisible();
		await expect(page.getByText("pane: %11", { exact: true })).toBeVisible();
		await expect(page.getByText("session key: agent:session:42", { exact: true })).toBeVisible();

		await expect(page.getByText("Attached to exact terminal terminal-agent-42.", { exact: true })).toBeVisible();
		await expect(page.getByLabel("Managed terminal output").locator(".xterm-rows")).toContainText(
			"exact tools service output",
		);

		const websocketUrl = new URL(
			websocketUrls.find((url) => new URL(url).searchParams.get("id") === "terminal-agent-42"),
		);
		expect(websocketUrl.pathname).toBe("/api/terminal/ws");
		expect(websocketUrl.searchParams.get("instanceId")).toBe(instanceId);
		expect(websocketUrl.searchParams.has("kind")).toBe(false);
		expect(websocketUrl.searchParams.get("id")).toBe("terminal-agent-42");
		expect(websocketUrl.searchParams.get("sessionKey")).toBe("agent:session:42");

		await page.getByLabel("Agent session").selectOption({ label: "agent:session:other" });
		await expect(page.getByRole("button", { name: /terminal-other-session-7/ })).toBeVisible();
		await expect(page.getByRole("button", { name: /terminal-agent-42/ })).toHaveCount(0);
		await expect(
			page.getByText("Attached to exact terminal terminal-other-session-7.", { exact: true }),
		).toBeVisible();
		await page.getByLabel("Agent session").selectOption({ label: "agent:session:42" });
		await expect(page.getByText("Attached to exact terminal terminal-agent-42.", { exact: true })).toBeVisible();

		await page.getByLabel("Managed terminal output").click();
		await page.keyboard.type("pwd");
		await page.getByRole("button", { name: "Clear", exact: true }).click();
		await expect.poll(() => clientMessages.some((message) => message.type === "resize")).toBeTruthy();
		await expect
			.poll(() => clientMessages.filter((message) => message.type === "input").map((message) => message.data).join(""))
			.toContain("pwd");
		await expect
			.poll(() => clientMessages.some((message) => message.type === "control" && message.action === "clear"))
			.toBeTruthy();

		const resizeCountBeforeLayoutChange = clientMessages.filter((message) => message.type === "resize").length;
		await page.setViewportSize({ width: 1180, height: 760 });
		await page.evaluate(
			() =>
				new Promise((resolve) => {
					requestAnimationFrame(() => requestAnimationFrame(resolve));
				}),
		);
		const resizeCountAfterLayoutChange = clientMessages.filter((message) => message.type === "resize").length;
		expect(resizeCountAfterLayoutChange - resizeCountBeforeLayoutChange).toBeLessThanOrEqual(2);
		await expect(page.getByRole("heading", { name: "Terminal", exact: true })).toBeVisible();

		await page.getByLabel("Session key for a new terminal").fill("agent:explicit:new");
		await page.getByRole("button", { name: "Create in selected service", exact: true }).click();
		await expect(page.getByLabel("Agent session").locator("option:checked")).toHaveText("agent:explicit:new");
		await expect(page.getByText("terminal: terminal-ui-created-99", { exact: true })).toBeVisible();
		await expect(page.getByText("session: $9", { exact: true })).toBeVisible();
		await expect(page.getByText("window: @12", { exact: true })).toBeVisible();
		await expect(page.getByText("pane: %15", { exact: true })).toBeVisible();
		expect(createRequests).toEqual([
			{
				url: expect.stringContaining(`/api/terminal/instances/${encodeURIComponent(instanceId)}/terminals`),
				body: { sessionKey: "agent:explicit:new" },
			},
		]);
		expect(pageErrors).toEqual([]);
	});
});