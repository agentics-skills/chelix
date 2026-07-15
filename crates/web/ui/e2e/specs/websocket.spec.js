const { expect, test } = require("../base-test");
const { expectRpcOk, navigateAndWait, sendRpcFromPage, waitForWsConnected, watchPageErrors } = require("../helpers");

async function clearChatAndWait(page) {
	await expectRpcOk(page, "chat.clear", {});
	await expect.poll(() => page.locator("#messages .msg").count(), { timeout: 10_000 }).toBe(0);
}
async function waitForChatSessionReady(page) {
	await page.waitForFunction(
		async () => {
			var appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) return false;
			var appUrl = new URL(appScript.src, window.location.origin);
			var prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			var state = await import(`${prefix}js/state.js`);
			return state.subscribed && !(state.sessionSwitchInProgress || state.chatBatchLoading);
		},
		{ timeout: 10_000 },
	);
}

async function mockRpcErrorResponse(page, method, message) {
	await page.evaluate(
		async ({ targetMethod, errorMessage }) => {
			var appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			var appUrl = new URL(appScript.src, window.location.origin);
			var prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			var stateModule = await import(`${prefix}js/state.js`);
			var ws = stateModule.ws;
			if (!ws) throw new Error("websocket unavailable");

			if (!window.__origWebsocketSpecWsSend) {
				window.__origWebsocketSpecWsSend = ws.send.bind(ws);
			}

			ws.send = (payload) => {
				try {
					var parsed = JSON.parse(payload);
					if (parsed?.method === targetMethod) {
						var resolver = stateModule.pending?.[parsed.id];
						if (typeof resolver === "function") {
							delete stateModule.pending[parsed.id];
							resolver({
								ok: false,
								error: {
									code: "INTERNAL",
									message: errorMessage,
								},
							});
						}
						return;
					}
				} catch (_err) {
					// Fall through to the original sender.
				}
				return window.__origWebsocketSpecWsSend(payload);
			};
		},
		{ targetMethod: method, errorMessage: message },
	);
}

async function mockRpcOkResponse(page, method, payload) {
	await page.evaluate(
		async ({ targetMethod, responsePayload }) => {
			var appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			var appUrl = new URL(appScript.src, window.location.origin);
			var prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			var stateModule = await import(`${prefix}js/state.js`);
			var ws = stateModule.ws;
			if (!ws) throw new Error("websocket unavailable");

			if (!window.__origWebsocketSpecWsSend) {
				window.__origWebsocketSpecWsSend = ws.send.bind(ws);
			}

			ws.send = (rawPayload) => {
				try {
					var parsed = JSON.parse(rawPayload);
					if (parsed?.method === targetMethod) {
						var resolver = stateModule.pending?.[parsed.id];
						if (typeof resolver === "function") {
							delete stateModule.pending[parsed.id];
							resolver({ ok: true, payload: responsePayload });
						}
						return;
					}
				} catch (_err) {
					// Fall through to the original sender.
				}
				return window.__origWebsocketSpecWsSend(rawPayload);
			};
		},
		{ targetMethod: method, responsePayload: payload },
	);
}
test.describe("WebSocket connection lifecycle", () => {
	test("status shows connected after page load", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/");
		await waitForWsConnected(page);

		await expect(page.locator("#statusDot")).toHaveClass(/connected/);
		// When connected, statusText is intentionally cleared to ""
		await expect(page.locator("#statusText")).toHaveText("");
		expect(pageErrors).toEqual([]);
	});

	test("chat.clear emits session_cleared chat event", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const events = await import(`${prefix}js/events.js`);

			window.__chatWsEvents = [];
			if (window.__chatWsEventsOff) {
				window.__chatWsEventsOff();
			}
			window.__chatWsEventsOff = events.onEvent("chat", (payload) => {
				window.__chatWsEvents.push(payload);
			});
		});

		await expectRpcOk(page, "chat.clear", {});

		await expect
			.poll(
				() =>
					page.evaluate(
						() =>
							window.__chatWsEvents.filter(
								(payload) => payload?.state === "session_cleared" && payload?.sessionKey === "main",
							).length,
					),
				{ timeout: 10_000 },
			)
			.toBeGreaterThan(0);

		await page.evaluate(() => {
			if (window.__chatWsEventsOff) {
				window.__chatWsEventsOff();
				window.__chatWsEventsOff = null;
			}
		});
		expect(pageErrors).toEqual([]);
	});

	test("tool call context budget updates token bar immediately and only from real metadata", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId: "context-budget-1",
				toolName: "execute_command",
				success: true,
				result: { stdout: "first", exit_code: 0 },
				contextBudget: {
					contextWindow: 200000,
					maxInputTokens: 180000,
					maxOutputTokens: 20000,
					compactionRatio: 85,
					promptTokens: 36125,
					toolSchemaTokens: 10000,
					availableInputTokens: 170000,
					compactionBudget: 144500,
					usagePercent: 25,
					compactionRequired: false,
				},
			},
		});
		await expect(page.locator("#tokenBar")).toContainText("[25%]");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId: "context-budget-2",
				toolName: "execute_command",
				success: true,
				result: { stdout: "second", exit_code: 0 },
			},
		});
		await expect(page.locator("#tokenBar")).toContainText("[25%]");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId: "context-budget-3",
				toolName: "execute_command",
				success: true,
				result: { stdout: "third", exit_code: 0 },
				contextBudget: {
					contextWindow: 200000,
					maxInputTokens: 180000,
					maxOutputTokens: 20000,
					compactionRatio: 85,
					promptTokens: 144500,
					toolSchemaTokens: 10000,
					availableInputTokens: 170000,
					compactionBudget: 144500,
					usagePercent: 100,
					compactionRequired: true,
				},
			},
		});
		await expect(page.locator("#tokenBar")).toContainText("[100%]");
		expect(pageErrors).toEqual([]);
	});

	test("session history restores context budget from the latest tool call", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);

		await page.route("**/api/sessions/history-budget/history*", async (route) => {
			await route.fulfill({
				status: 200,
				contentType: "application/json",
				body: JSON.stringify({
					historyCacheHit: false,
					historyTruncated: false,
					history: [
						{
							role: "tool_result",
							tool_call_id: "history-budget-1",
							tool_name: "execute_command",
							success: true,
							result: { stdout: "first", exit_code: 0 },
							contextBudget: {
								contextWindow: 200000,
								maxInputTokens: 180000,
								maxOutputTokens: 20000,
								compactionRatio: 85,
								promptTokens: 36125,
								toolSchemaTokens: 10000,
								availableInputTokens: 170000,
								compactionBudget: 144500,
								usagePercent: 25,
								compactionRequired: false,
							},
						},
						{
							role: "tool_result",
							tool_call_id: "history-budget-2",
							tool_name: "execute_command",
							success: true,
							result: { stdout: "second", exit_code: 0 },
							contextBudget: {
								contextWindow: 200000,
								maxInputTokens: 180000,
								maxOutputTokens: 20000,
								compactionRatio: 85,
								promptTokens: 72250,
								toolSchemaTokens: 10000,
								availableInputTokens: 170000,
								compactionBudget: 144500,
								usagePercent: 50,
								compactionRequired: false,
							},
						},
					],
				}),
			});
		});
		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "history-budget", messageCount: 2 },
			historyOmitted: true,
			replying: false,
		});
		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("history-budget");
		});

		await expect(page.locator("#tokenBar")).toContainText("[50%]");
		expect(pageErrors).toEqual([]);
	});

	test("memory info updates from tick events", async ({ page }) => {
		await page.goto("/");
		await waitForWsConnected(page);

		// tick events carry memory stats; wait for memoryInfo to populate
		await expect(page.locator("#memoryInfo")).not.toHaveText("", {
			timeout: 15_000,
		});
	});

	test("connection persists across SPA navigation", async ({ page }) => {
		await page.goto("/");
		await waitForWsConnected(page);

		// Navigate to a different page within the SPA
		await page.goto("/settings");
		await expect(page.locator("#pageContent")).not.toBeEmpty();

		// WebSocket should remain connected through client-side navigation
		await expect(page.locator("#statusDot")).toHaveClass(/connected/);

		// Navigate back to chat
		await page.goto("/chats/main");
		await expect(page.locator("#pageContent")).not.toBeEmpty();
		await expect(page.locator("#statusDot")).toHaveClass(/connected/);
	});

	test("health endpoint responds", async ({ request }) => {
		// Verify the server is healthy via the HTTP health endpoint
		const resp = await request.get("/health");
		expect(resp.ok()).toBeTruthy();
	});

	test("RPC timeouts identify the slow method instead of reporting disconnect", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const warnings = [];
		page.on("console", (msg) => {
			if (msg.type() === "warning") warnings.push(msg.text());
		});
		await page.goto("/");
		await waitForWsConnected(page);

		const res = await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");

			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const helpers = await import(`${prefix}js/helpers.js`);
			const state = await import(`${prefix}js/state.js`);
			const originalWs = state.ws;
			const originalTimeout = window.__chelixTestRpcTimeoutMs;

			try {
				window.__chelixTestRpcTimeoutMs = 1_000;
				state.setWs({
					readyState: WebSocket.OPEN,
					send() {
						// Intentionally never resolves; this exercises the client timeout path.
					},
				});

				return await helpers.sendRpc("test.slow_method", {});
			} finally {
				state.setWs(originalWs);
				window.__chelixTestRpcTimeoutMs = originalTimeout;
			}
		});

		expect(res).toMatchObject({
			ok: false,
			error: {
				code: "TIMEOUT",
			},
		});
		expect(res.error.message).toContain("test.slow_method");
		expect(res.error.message).not.toContain("WebSocket disconnected");
		expect(warnings.some((warning) => warning.includes("RPC request timed out"))).toBeTruthy();
		expect(warnings.some((warning) => warning.includes("test.slow_method"))).toBeTruthy();
		expect(pageErrors).toEqual([]);
	});

	test("final chat text is kept when it includes tool output plus analysis", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolOutput = "Linux chelix-chelix-sandbox-main 6.12.28 #1 SMP Tue May 20 15:19:05 UTC 2025 aarch64 GNU/Linux";
		const finalText =
			"The command executed successfully. The output shows:\n- Kernel name: Linux\n- Hostname: chelix-chelix-sandbox-main\n\n" +
			toolOutput;

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId: "echo-test",
				success: true,
				result: { stdout: toolOutput, stderr: "", exit_code: 0 },
			},
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "delta",
				text: finalText,
			},
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: finalText,
				messageIndex: 999,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
			},
		});

		await expect(
			page.locator("#messages .msg.assistant").filter({ hasText: "command executed successfully" }),
		).toBeVisible();
		await expect(
			page.locator("#messages .msg.assistant").filter({ hasText: "chelix-chelix-sandbox-main" }),
		).toBeVisible();
		expect(pageErrors).toEqual([]);
	});

	test("markdown and ansi tables render as structured HTML tables", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await clearChatAndWait(page);

		const markdownTableText = [
			"Here are nearby cafes:",
			"",
			"| # | Cafe | Rating |",
			"|---|------|--------|",
			"| 1 | **Mellis Cafe** | ⭐4.8 |",
			"| 2 | **Scullery** | ⭐4.7 |",
		].join("\n");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: markdownTableText,
				messageIndex: 999905,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
			},
		});

		const markdownAssistant = page.locator("#messages .msg.assistant").last();
		const markdownTable = markdownAssistant.locator("table.msg-table");
		await expect(markdownTable).toHaveCount(1);
		await expect(markdownTable.locator("thead th")).toHaveText(["#", "Cafe", "Rating"]);
		await expect(markdownTable.locator("tbody tr")).toHaveCount(2);
		await expect(markdownTable.locator("tbody tr").first().locator("strong")).toHaveText("Mellis Cafe");

		const ansiTableText = [
			"Same data from an ANSI output table:",
			"",
			"\u001b[32m+----+--------------------+\u001b[0m",
			"\u001b[32m| #  | Cafe               |\u001b[0m",
			"\u001b[32m+----+--------------------+\u001b[0m",
			"\u001b[32m| 1  | Mellis Cafe        |\u001b[0m",
			"\u001b[32m| 2  | The Coffee Movement |\u001b[0m",
			"\u001b[32m+----+--------------------+\u001b[0m",
		].join("\n");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: ansiTableText,
				messageIndex: 999906,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
			},
		});

		const ansiAssistant = page.locator("#messages .msg.assistant").last();
		const ansiTable = ansiAssistant.locator("table.msg-table");
		await expect(ansiTable).toHaveCount(1);
		await expect(ansiTable.locator("thead th")).toHaveText(["#", "Cafe"]);
		await expect(ansiTable.locator("tbody tr")).toHaveCount(2);
		await expect(ansiAssistant).not.toContainText("\u001b[");
		expect(pageErrors).toEqual([]);
	});

	test("final footer shows token speed with slow/fast tones", async ({ page }) => {
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await clearChatAndWait(page);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "slow reply",
				messageIndex: 999903,
				model: "test-model",
				provider: "test-provider",
				inputTokens: 100,
				outputTokens: 6,
				durationMs: 3000,
				replyMedium: "text",
			},
		});

		const slowAssistant = page.locator("#messages .msg.assistant").last();
		await expect(slowAssistant.locator(".msg-token-speed.msg-token-speed-slow")).toContainText("tok/s");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "fast reply",
				messageIndex: 999904,
				model: "test-model",
				provider: "test-provider",
				inputTokens: 120,
				outputTokens: 90,
				durationMs: 2000,
				replyMedium: "text",
			},
		});

		const fastAssistant = page.locator("#messages .msg.assistant").last();
		await expect(fastAssistant.locator(".msg-token-speed.msg-token-speed-fast")).toContainText("tok/s");
	});

	test("voice fallback action and warning render for voice final without audio", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await clearChatAndWait(page);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "voice fallback should be available",
				messageIndex: 999901,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "voice",
				audioWarning: "TTS synthesis failed: timeout",
			},
		});

		var assistant = page.locator("#messages .msg.assistant").last();
		await expect(assistant).toContainText("voice fallback should be available");
		await expect(assistant.locator(".msg-voice-warning")).toContainText("timeout");
		// Voice action is now an icon button in the action bar
		await expect(assistant.locator('.msg-action-btn[title="Voice it"]')).toBeVisible();
		expect(pageErrors).toEqual([]);
	});

	test("voice fallback action shows error when generation RPC fails", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await clearChatAndWait(page);
		await mockRpcErrorResponse(page, "sessions.voice.generate", "Voice generation failed for test.");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "try generating voice now",
				messageIndex: 999902,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "voice",
			},
		});

		var assistant = page.locator("#messages .msg.assistant").last();
		await expect(assistant).toContainText("try generating voice now");
		var voiceBtn = assistant.locator('.msg-action-btn[title="Voice it"]');
		await expect(voiceBtn).toBeVisible();
		await voiceBtn.click();
		// After failed RPC the button title reverts and a toast is shown
		await expect(voiceBtn).toHaveAttribute("title", "Voice it");
		expect(pageErrors).toEqual([]);
	});

	test("voice fallback action shows generated TTS provider", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await clearChatAndWait(page);
		await mockRpcOkResponse(page, "sessions.voice.generate", {
			audio: "media/main/voice-msg-999903.ogg",
			ttsProvider: "openai",
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "generate provider metadata",
				messageIndex: 999903,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "voice",
			},
		});

		var assistant = page.locator("#messages .msg.assistant").last();
		var voiceBtn = assistant.locator('.msg-action-btn[title="Voice it"]');
		await expect(voiceBtn).toBeVisible();
		await voiceBtn.click();
		await expect(assistant.locator(".msg-tts-provider-footer")).toContainText("TTS: OpenAI TTS (openai)");
		expect(pageErrors).toEqual([]);
	});

	test("final event is rendered even if switchInProgress gets stuck", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await expectRpcOk(page, "chat.clear", {});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessionStoreModule = await import(`${prefix}js/stores/session-store.js`);
			const stateModule = await import(`${prefix}js/state.js`);
			sessionStoreModule.sessionStore.switchInProgress.value = true;
			stateModule.setSessionSwitchInProgress(true);
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "render this final despite stale switch flag",
				messageIndex: 991001,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
				runId: "run-stuck-switch-final",
			},
		});

		await expect(
			page.locator("#messages .msg.assistant").filter({ hasText: "render this final despite stale switch flag" }),
		).toBeVisible();
		await expect
			.poll(() =>
				page.evaluate(async () => {
					const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
					if (!appScript) return null;
					const appUrl = new URL(appScript.src, window.location.origin);
					const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
					const sessionStoreModule = await import(`${prefix}js/stores/session-store.js`);
					return sessionStoreModule.sessionStore.switchInProgress.value;
				}),
			)
			.toBe(false);

		expect(pageErrors).toEqual([]);
	});

	test("out-of-order tool events still resolve command card", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolCallId = "reorder-command-1";
		const fullOutputPath = "/root/.chelix/sessions/tool-results/session_test/call_test/content.txt";
		const truncatedResult =
			'{"background":false,"completed":true,"exitCode":0,"message":"Command finished","output":"Line 1: XXXXX [counter=1]\\nLine 2: XXXXX [counter=2]\\nLine 3: XXXXX [counter=3]' +
			`\n\n[Truncated — full tool result (101KB) written to file. Use the Read tool to access the content at: ${fullOutputPath}]`;
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId,
				toolName: "execute_command",
				success: true,
				result: truncatedResult,
			},
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				toolCallId,
				toolName: "execute_command",
				arguments: { command: "df -h" },
			},
		});

		const card = page.locator(`#tool-${toolCallId}`);
		await expect(card).toBeVisible();
		await expect(card).toHaveClass(/command-ok/);
		await expect(page.locator(`#tool-${toolCallId} .command-status`)).toHaveCount(0);
		const output = page.locator(`#tool-${toolCallId} .command-output`);
		await expect(output).toContainText("Line 1: XXXXX [counter=1]");
		await expect(output).toContainText("Line 3: XXXXX [counter=3]");
		await expect(output).toContainText("[Truncated — full tool result (101KB) written to file.");
		await expect(output).toContainText(fullOutputPath);
		await expect(output).not.toContainText('{"background":false');
		expect(pageErrors).toEqual([]);
	});

	test("switch payload restores active running tool calls", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:active-tool", messageCount: 1 },
			historyOmitted: false,
			history: [{ role: "user", content: "run a command", historyIndex: 0 }],
			replying: true,
			activeToolCalls: [
				{
					runId: "run-switch-tool",
					toolCallId: "tc-switch-tool",
					toolName: "execute_command",
					arguments: { command: "sleep 10" },
				},
			],
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:active-tool");
		});

		const card = page.locator("#tool-run-switch-tool-tc-switch-tool");
		await expect(card).toBeVisible();
		await expect(card.locator(".tool-call-result-placeholder")).toContainText("Waiting for tool result");
		expect(pageErrors).toEqual([]);
	});

	test("history renders model metadata only on terminal assistant segments", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:terminal-metadata", messageCount: 4 },
			historyOmitted: false,
			history: [
				{ role: "user", content: "run tools", historyIndex: 0 },
				{
					role: "assistant",
					content: "Before tools.",
					model: "mock-model",
					provider: "mock",
					inputTokens: 20,
					outputTokens: 3,
					tool_calls: [{ id: "tool-1", name: "execute_command" }],
					historyIndex: 1,
				},
				{
					role: "tool_result",
					tool_call_id: "tool-1",
					tool_name: "execute_command",
					success: true,
					historyIndex: 2,
				},
				{
					role: "assistant",
					content: "Final answer.",
					model: "mock-model",
					provider: "mock",
					inputTokens: 30,
					outputTokens: 8,
					durationMs: 100,
					historyIndex: 3,
				},
			],
			replying: false,
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:terminal-metadata");
		});

		const preTool = page.locator('.msg.assistant[data-history-index="1"]');
		const terminal = page.locator('.msg.assistant[data-history-index="3"]');
		await expect(preTool).toContainText("Before tools.");
		await expect(preTool.locator(".msg-model-footer")).toHaveCount(0);
		await expect(terminal).toContainText("Final answer.");
		await expect(terminal.locator(".msg-model-footer")).toHaveCount(0);
		await expect(page.locator('.terminal-metadata[data-history-index="3"]')).toContainText("mock / mock-model");

		expect(pageErrors).toEqual([]);
	});

	test("history renders terminal empty tool metadata as a standalone row", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:terminal-empty-tool", messageCount: 3 },
			historyOmitted: false,
			history: [
				{ role: "user", content: "run a tool", historyIndex: 0 },
				{
					role: "assistant",
					content: "",
					model: "mock-model",
					provider: "mock",
					inputTokens: 17,
					outputTokens: 9,
					cacheReadTokens: 3,
					durationMs: 200,
					created_at: 1_700_000_000_000,
					tool_calls: [{ id: "tool-empty-terminal", name: "execute_command" }],
					historyIndex: 1,
				},
				{
					role: "tool_result",
					tool_call_id: "tool-empty-terminal",
					tool_name: "execute_command",
					success: false,
					created_at: 1_700_000_000_000,
					historyIndex: 2,
				},
			],
			replying: false,
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:terminal-empty-tool");
		});

		const toolCard = page.locator('[data-tool-call-id="tool-empty-terminal"]');
		await expect(toolCard.locator(".msg-model-footer")).toHaveCount(0);
		const metadata = page.locator('.terminal-metadata[data-history-index="1"]');
		await expect(metadata).toHaveCount(1);
		await expect(metadata).toContainText("mock / mock-model");
		await expect(metadata).toContainText("17 in (3 cached) / 9 out");
		await expect(metadata.locator(".msg-footer-time")).toHaveCount(1);
		await expect(page.locator('.msg.assistant[data-history-index="1"]')).toHaveCount(0);
		expect(pageErrors).toEqual([]);
	});

	test("user_message during switch is cached and rendered in child sessions", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:child-live", messageCount: 1 },
			historyOmitted: false,
			history: [],
			replying: false,
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:child-live");
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "session:child-live",
				state: "user_message",
				text: "prompt sent to child",
				messageIndex: 0,
			},
		});

		await expect(page.locator("#messages .msg.user").filter({ hasText: "prompt sent to child" })).toBeVisible();
		expect(pageErrors).toEqual([]);
	});

	test("final event clears stale running command status when tool end is missed", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolCallId = "stale-command-1";
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				toolCallId,
				toolName: "execute_command",
				arguments: { command: "df -h" },
			},
		});

		await expect(page.locator(`#tool-${toolCallId} .command-status`)).toBeVisible();

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "done",
				messageIndex: 999999,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
			},
		});

		await expect(page.locator(`#tool-${toolCallId} .command-status`)).toHaveCount(0);
		await expect(page.locator(`#tool-${toolCallId}`)).toHaveClass(/command-ok/);
		expect(pageErrors).toEqual([]);
	});

	test("map links render place name with right-side rating details", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolCallId = "map-links-icons-1";
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				toolCallId,
				toolName: "show_map",
				arguments: { label: "Tartine Bakery ⭐4.7 - Open till 4PM" },
			},
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId,
				toolName: "show_map",
				success: true,
				result: {
					label: "Tartine Bakery ⭐4.7 - Open till 4PM",
					map_links: {
						provider: "google_maps",
						url: "https://www.google.com/maps/search/?api=1&query=Tartine+Bakery&center=37.7615,-122.4241",
						google_maps: "https://www.google.com/maps/search/?api=1&query=Tartine+Bakery&center=37.7615,-122.4241",
					},
				},
			},
		});

		const card = page.locator(`#tool-${toolCallId}`);
		await expect(card).toBeVisible();
		await expect(card.locator("img.map-service-icon")).toHaveCount(0);
		const mapLink = card.locator("a.map-link-row");
		await expect(mapLink).toHaveCount(1);
		await expect(mapLink.locator(".map-link-name")).toHaveText("Tartine Bakery");
		await expect(mapLink.locator(".map-link-meta")).toHaveText("⭐4.7 - Open till 4PM");
		await expect(mapLink).toHaveAttribute("title", 'Open "Tartine Bakery ⭐4.7 - Open till 4PM" in maps');
		await expect(card.locator('a:has-text("Tartine Bakery")')).toHaveCount(1);
		expect(pageErrors).toEqual([]);
	});

	test("map links render per-point groups when show_map returns points", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolCallId = "map-links-points-1";
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				toolCallId,
				toolName: "show_map",
				arguments: { label: "Breakfast spots" },
			},
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				toolCallId,
				toolName: "show_map",
				success: true,
				result: {
					label: "Breakfast spots",
					map_links: {
						provider: "google_maps",
						url: "https://www.google.com/maps/search/?api=1&query=Breakfast+spots&center=37.788473,-122.408997",
						google_maps: "https://www.google.com/maps/search/?api=1&query=Breakfast+spots&center=37.788473,-122.408997",
					},
					points: [
						{
							label: "Sears Fine Food",
							latitude: 37.788473,
							longitude: -122.408997,
							map_links: {
								provider: "google_maps",
								url: "https://www.google.com/maps/search/?api=1&query=Sears+Fine+Food&center=37.788473,-122.408997",
								google_maps:
									"https://www.google.com/maps/search/?api=1&query=Sears+Fine+Food&center=37.788473,-122.408997",
							},
						},
						{
							label: "Surisan",
							latitude: 37.80895,
							longitude: -122.41576,
							map_links: {
								provider: "google_maps",
								url: "https://www.google.com/maps/search/?api=1&query=Surisan&center=37.80895,-122.41576",
								google_maps: "https://www.google.com/maps/search/?api=1&query=Surisan&center=37.80895,-122.41576",
							},
						},
					],
				},
			},
		});

		const card = page.locator(`#tool-${toolCallId}`);
		await expect(card).toBeVisible();
		await expect(card.locator("img.map-service-icon")).toHaveCount(0);
		await expect(card.locator('a:has-text("Sears Fine Food")')).toHaveCount(1);
		await expect(card.locator('a:has-text("Surisan")')).toHaveCount(1);
		await expect(card.locator('a[title="Open \\"Sears Fine Food\\" in maps"]')).toHaveCount(1);
		await expect(card.locator('a[title="Open \\"Surisan\\" in maps"]')).toHaveCount(1);
		expect(pageErrors).toEqual([]);
	});

	test("thinking text is preserved as reasoning disclosure when tool call follows", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);

		await expectRpcOk(page, "chat.clear", {});

		// 1. thinking indicator appears
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: { sessionKey: "main", state: "thinking", runId: "run-think-tool" },
		});
		await expect(page.locator("#thinkingIndicator")).toBeVisible();

		// 2. thinking text arrives
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "thinking_text",
				runId: "run-think-tool",
				text: "I need to search the web for recent news",
			},
		});
		await expect(page.locator("#thinkingIndicator .thinking-text")).toContainText("I need to search the web");

		// 3. thinking_done — indicator should NOT be removed yet
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: { sessionKey: "main", state: "thinking_done", runId: "run-think-tool" },
		});
		await expect(page.locator("#thinkingIndicator")).toBeVisible();

		// 4. tool_call_start — thinking text is preserved as disclosure, indicator removed
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-think-tool",
				toolCallId: "tc-web-search-1",
				toolName: "web_search",
				arguments: { query: "top news today" },
			},
		});
		await expect(page.locator("#thinkingIndicator")).toHaveCount(0);
		// Reasoning disclosure is inside the tool card
		const toolCard = page.locator("#tool-run-think-tool-tc-web-search-1");
		await expect(toolCard).toBeVisible();
		await expect(toolCard.locator(".msg-reasoning")).toBeVisible();
		await expect(toolCard.locator(".msg-reasoning-body")).toContainText("I need to search the web for recent news");

		// 5. final with same reasoning should NOT duplicate the disclosure
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "Here are the top news stories.",
				messageIndex: 999998,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
				reasoning: "I need to search the web for recent news",
			},
		});
		// Only one reasoning disclosure should exist (the preserved one, not a duplicate)
		await expect(page.locator(".msg-reasoning")).toHaveCount(1);
		expect(pageErrors).toEqual([]);
	});

	test("whitespace-only streamed assistant bubble is removed once tool call starts/finalizes", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await expectRpcOk(page, "chat.clear", {});

		// Simulate an assistant stream that emits only whitespace before deciding to call a tool.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "delta",
				runId: "run-whitespace-tool",
				text: " \n\t ",
			},
		});
		await expect(page.locator("#messages .msg.assistant")).toHaveCount(0);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-whitespace-tool",
				toolCallId: "tc-empty-1",
				toolName: "execute_command",
				arguments: { command: "echo $FOO" },
			},
		});

		const toolCard = page.locator("#tool-run-whitespace-tool-tc-empty-1");
		await expect(toolCard).toBeVisible();
		await expect(page.locator("#messages .msg.assistant")).toHaveCount(0);

		// Final text is also whitespace-only. No empty assistant bubble should be left behind.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				runId: "run-whitespace-tool",
				text: "\n  \t",
				messageIndex: 999997,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
			},
		});

		await expect(page.locator("#messages .msg.assistant")).toHaveCount(0);
		await expect(toolCard.locator(".msg-model-footer")).toHaveCount(0);
		await expect(page.locator('.terminal-metadata[data-history-index="999997"]')).toHaveCount(1);
		expect(pageErrors).toEqual([]);
	});

	test("auth.credentials_changed event redirects through /login", async ({ page }) => {
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		var loginNavigation = page.waitForRequest(
			(request) => request.isNavigationRequest() && new URL(request.url()).pathname === "/login",
			{ timeout: 10_000 },
		);

		// Inject the auth.credentials_changed event via system-event RPC.
		await sendRpcFromPage(page, "system-event", {
			event: "auth.credentials_changed",
			payload: { reason: "test_disconnect" },
		});

		// The event handler should trigger a navigation to /login.
		await loginNavigation;

		// In local no-password mode, /login immediately routes back to chat.
		await expect.poll(() => new URL(page.url()).pathname).toMatch(/^\/(?:login|chats\/.+)$/);
	});

	test("UNAUTHORIZED redirect guard resets after auth sync completes", async ({ page }) => {
		const pageErrors = watchPageErrors(page);

		await page.addInitScript(() => {
			const originalFetch = window.fetch.bind(window);
			window.fetch = (...args) => {
				const url = typeof args[0] === "string" ? args[0] : args[0]?.url || "";
				if (url.endsWith("/api/auth/status")) {
					return Promise.resolve(
						new Response(
							JSON.stringify({
								authenticated: false,
								setup_required: false,
								auth_disabled: false,
								localhost_only: false,
								has_password: true,
								has_passkeys: false,
							}),
							{
								status: 200,
								headers: { "Content-Type": "application/json" },
							},
						),
					);
				}
				return originalFetch(...args);
			};
		});

		await page.goto("/login");
		await page.waitForLoadState("domcontentloaded");

		const counts = await page.evaluate(async () => {
			const loginScript = document.querySelector('script[type="module"][src*="js/login-app.js"]');
			if (!loginScript) throw new Error("login module script not found");

			const loginUrl = new URL(loginScript.src, window.location.origin);
			const prefix = loginUrl.href.slice(0, loginUrl.href.length - "js/login-app.js".length);

			class FakeWebSocket {
				constructor(url) {
					this.url = url;
					this.sent = [];
					FakeWebSocket.instance = this;
				}

				send(data) {
					this.sent.push(JSON.parse(data));
				}

				close() {
					// Fake WebSocket used only for unit-style module testing.
				}
			}

			const originalWebSocket = window.WebSocket;
			window.WebSocket = FakeWebSocket;
			window.__authChangedEvents = 0;
			window.addEventListener("chelix:auth-status-changed", () => {
				window.__authChangedEvents += 1;
			});

			try {
				const wsModule = await import(`${prefix}js/ws-connect.js?e2e=${Date.now()}`);
				wsModule.connectWs({});

				const ws = FakeWebSocket.instance;
				if (!ws) throw new Error("fake websocket was not created");
				ws.onopen();

				const connectFrame = ws.sent.find((frame) => frame.method === "connect");
				if (!connectFrame) throw new Error("connect frame was not sent");

				ws.onmessage({
					data: JSON.stringify({
						type: "res",
						id: connectFrame.id,
						ok: true,
						payload: { type: "hello-ok" },
					}),
				});

				const unauthorizedFrame = JSON.stringify({
					type: "res",
					id: "unauthorized-1",
					ok: false,
					error: { code: "UNAUTHORIZED", message: "expired" },
				});

				ws.onmessage({ data: unauthorizedFrame });
				const afterFirst = window.__authChangedEvents;

				ws.onmessage({ data: unauthorizedFrame });
				const afterBurst = window.__authChangedEvents;

				window.dispatchEvent(new CustomEvent("chelix:auth-status-sync-complete"));

				ws.onmessage({ data: unauthorizedFrame });
				return {
					afterFirst,
					afterBurst,
					afterReset: window.__authChangedEvents,
				};
			} finally {
				window.WebSocket = originalWebSocket;
			}
		});

		expect(counts).toEqual({
			afterFirst: 1,
			afterBurst: 1,
			afterReset: 2,
		});
		expect(pageErrors).toEqual([]);
	});
});
