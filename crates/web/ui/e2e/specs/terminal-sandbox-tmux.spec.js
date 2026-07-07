const { expect, test } = require("../base-test");
const { navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

function sandboxTmuxTreePayload() {
	return {
		tree: {
			available: true,
			reason: null,
			sessions: [
				{
					id: "$agent",
					name: "agent-main",
					attached: false,
					windows: [
						{
							id: "@7",
							index: 2,
							name: "builder",
							active: true,
							panes: [
								{
									id: "%11",
									index: 0,
									active: true,
									currentCommand: "bash",
									currentPath: "/home/sandbox",
									title: "agent",
								},
							],
						},
					],
				},
			],
		},
	};
}

test.describe("Terminal sandbox tmux targets", () => {
	test("lists sandbox tmux windows and attaches selected target through sandbox websocket", async ({ page }) => {
		const errors = watchPageErrors(page);
		const terminalWebSocketUrls = [];

		await page.route("/api/terminal/sandbox/targets", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({
					targets: [
						{
							id: "docker:moltis-sandbox-test",
							label: "moltis-sandbox-test (docker)",
							backend: "docker",
							containerName: "moltis-sandbox-test",
							state: "running",
							image: "moltis/sandbox:test",
						},
					],
				}),
			});
		});
		await page.route("/api/terminal/sandbox/tmux-tree?**", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify(sandboxTmuxTreePayload()),
			});
		});
		await page.route("/api/terminal/windows", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({ available: true, windows: [], activeWindowId: null }),
			});
		});
		await page.routeWebSocket("/api/terminal/sandbox/ws?**", (ws) => {
			ws.send(
				JSON.stringify({
					type: "ready",
					available: true,
					mode: "sandbox_tmux",
					persistenceEnabled: true,
					persistenceAvailable: true,
					targetLabel: "moltis-sandbox-test (docker)",
					sessionId: "$agent",
					windowId: "@7",
					paneId: "%11",
				}),
			);
		});
		page.on("websocket", (ws) => {
			const url = ws.url();
			if (url.includes("/api/terminal/sandbox/ws")) terminalWebSocketUrls.push(url);
		});

		await navigateAndWait(page, "/settings/terminal");
		await waitForWsConnected(page);

		await expect(page.locator("#terminalTarget")).toContainText("moltis-sandbox-test (docker)");
		await page.locator("#terminalTarget").selectOption("sandbox:docker:moltis-sandbox-test");

		await expect(page.getByRole("button", { name: "agent-main / 2: builder" })).toBeVisible({ timeout: 10_000 });
		await expect(page.locator("#terminalMeta")).toContainText("Sandbox tmux: moltis-sandbox-test (docker)");
		await expect(page.locator("#terminalHint")).toContainText(
			"Attached to a real tmux session inside the selected sandbox.",
		);

		await expect
			.poll(() => terminalWebSocketUrls.find((url) => url.includes("/api/terminal/sandbox/ws")) || "", {
				timeout: 10_000,
			})
			.toContain("targetId=docker%3Amoltis-sandbox-test");
		const terminalUrl = terminalWebSocketUrls.find((url) => url.includes("/api/terminal/sandbox/ws")) || "";
		expect(terminalUrl).toContain("sessionId=%24agent");
		expect(terminalUrl).toContain("windowId=%407");
		expect(terminalUrl).toContain("paneId=%2511");
		expect(errors).toHaveLength(0);
	});
});
