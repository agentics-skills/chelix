const { expect, test } = require("../base-test");
const { navigateAndWait, waitForWsConnected } = require("../helpers");

// Install a WebSocket send() shim that answers chat.context and
// chat.full_context locally, so the Context/debug surfaces can be exercised
// deterministically without a live provider. Mirrors the mocking approach used
// by chat-input.spec.js.
async function mockContextRpcs(page) {
	await page.evaluate(async () => {
		const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
		if (!appScript) throw new Error("app module script not found");
		const appUrl = new URL(appScript.src, window.location.origin);
		const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
		const stateModule = await import(`${prefix}js/state.js`);
		const ws = stateModule.ws;
		if (!ws) throw new Error("websocket unavailable");
		if (!window.__origCtxWsSend) window.__origCtxWsSend = ws.send.bind(ws);

		function resolvePending(id, payload) {
			const resolver = stateModule.pending?.[id];
			if (typeof resolver !== "function") return false;
			delete stateModule.pending[id];
			resolver({ ok: true, payload });
			return true;
		}

		function handle(parsed) {
			if (parsed?.method === "chat.context") {
				// A lazy-mode registry: the catalog advertises every tool
				// (including get_tool), but only one schema is currently visible.
				return resolvePending(parsed.id, {
					session: { key: "main", messageCount: 2, model: "demo-model" },
					supportsTools: true,
					tools: [
						{ name: "Edit", description: "Edit a file" },
						{ name: "get_tool", description: "Fetch a tool schema by name" },
						{ name: "memory_search", description: "Search long-term memory" },
					],
					toolSchemaCount: 1,
					sandbox: { enabled: false, backend: null },
				});
			}
			if (parsed?.method === "chat.full_context") {
				return resolvePending(parsed.id, {
					messageCount: 2,
					systemPromptChars: 64,
					totalChars: 128,
					messages: [
						{
							role: "system",
							content:
								'## Available Tools\n\n- `{"name":"Edit"}`: Edit a file\n- `{"name":"get_tool"}`: Fetch a tool schema by name\n',
						},
						{ role: "user", content: "hello" },
					],
					llmOutputs: [],
				});
			}
			return false;
		}

		ws.send = (payload) => {
			try {
				const parsed = JSON.parse(payload);
				if (handle(parsed)) return;
			} catch (_err) {
				// Not a JSON RPC frame — fall through to the real sender.
			}
			return window.__origCtxWsSend(payload);
		};
	});
}

test.describe("Tool catalog context surfaces", () => {
	test("debug context shows the full tool catalog and lazy schema count", async ({ page }) => {
		const pageErrors = await navigateAndWait(page, "/");
		await waitForWsConnected(page, 10_000).catch(() => "ignored");
		await mockContextRpcs(page);

		await page.locator("#debugPanelBtn").click();
		await expect(page.locator("#debugModal")).toBeVisible({ timeout: 10_000 });

		const panel = page.locator("#debugPanel");
		// The catalog lists every allowed tool — including get_tool — not just
		// the tools whose schemas are visible.
		await expect(panel).toContainText("get_tool", { timeout: 10_000 });
		await expect(panel).toContainText("Edit");
		await expect(panel).toContainText("memory_search");
		// Lazy schema visibility is surfaced separately from the catalog.
		await expect(panel).toContainText("1 of 3 tool schemas loaded (lazy mode)");

		expect(pageErrors).toEqual([]);
	});

	test("full context renders JSON-name Available Tools labels", async ({ page }) => {
		const pageErrors = await navigateAndWait(page, "/");
		await waitForWsConnected(page, 10_000).catch(() => "ignored");
		await mockContextRpcs(page);

		await page.locator("#fullContextBtn").click();
		await expect(page.locator("#fullContextModal")).toBeVisible({ timeout: 10_000 });

		// The system prompt advertises tools with JSON-name labels; the system
		// message renders collapsed, so read textContent directly.
		await expect
			.poll(async () => (await page.locator("#fullContextPanel").textContent()) || "", { timeout: 10_000 })
			.toContain('{"name":"Edit"}');
		const panelText = (await page.locator("#fullContextPanel").textContent()) || "";
		expect(panelText).toContain('{"name":"get_tool"}');

		expect(pageErrors).toEqual([]);
	});
});
