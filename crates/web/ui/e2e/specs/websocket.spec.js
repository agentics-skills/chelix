const { expect, test } = require("../base-test");
const { expectRpcOk, navigateAndWait, sendRpcFromPage, waitForWsConnected, watchPageErrors } = require("../helpers");

async function clearChatAndWait(page) {
	await expectRpcOk(page, "chat.clear", {});
	await expect.poll(() => page.locator("#messages .msg").count(), { timeout: 10_000 }).toBe(0);
}
function lifecycleEvent(stage, fields) {
	const { sequence = 0, emittedAtMs = 1_700_000_000_000, ...eventFields } = fields;
	return {
		...eventFields,
		sequence,
		emittedAtMs,
		stage,
	};
}

function liveToolLifecycle(stage, fields) {
	return { state: "tool_lifecycle", ...lifecycleEvent(stage, fields) };
}

function persistedToolLifecycle(stage, fields) {
	return { role: "tool_lifecycle", ...lifecycleEvent(stage, fields) };
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
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId: "context-budget-1",
				toolName: "execute_command",
				sequence: 8,
				arguments: {},
				success: true,
				result: JSON.stringify({ stdout: "first", exit_code: 0 }),
				error: null,
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
			}),
		});
		await expect(page.locator("#tokenBar")).toContainText("[25%]");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId: "context-budget-2",
				toolName: "execute_command",
				sequence: 8,
				arguments: {},
				success: true,
				result: JSON.stringify({ stdout: "second", exit_code: 0 }),
				error: null,
			}),
		});
		await expect(page.locator("#tokenBar")).toContainText("[25%]");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId: "context-budget-3",
				toolName: "execute_command",
				sequence: 8,
				arguments: {},
				success: true,
				result: JSON.stringify({ stdout: "third", exit_code: 0 }),
				error: null,
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
			}),
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
						persistedToolLifecycle("completed", {
							toolCallId: "history-budget-1",
							toolName: "execute_command",
							sequence: 8,
							arguments: {},
							success: true,
							result: JSON.stringify({ stdout: "first", exit_code: 0 }),
							error: null,
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
						}),
						persistedToolLifecycle("completed", {
							toolCallId: "history-budget-2",
							toolName: "execute_command",
							sequence: 8,
							arguments: {},
							success: true,
							result: JSON.stringify({ stdout: "second", exit_code: 0 }),
							error: null,
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
						}),
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
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId: "echo-test",
				toolName: "execute_command",
				sequence: 8,
				arguments: { command: "uname -a" },
				success: true,
				result: JSON.stringify({ stdout: toolOutput, stderr: "", exit_code: 0 }),
				error: null,
			}),
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

	test("tool lifecycle streams parameters and uses backend execution progress", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await expectRpcOk(page, "chat.clear", {});

		const runId = "run-live-lifecycle";
		const toolCallId = "live-lifecycle-command";
		const base = {
			sessionKey: "main",
			runId,
			toolCallId,
			toolName: "execute_command",
		};
		const sendLifecycle = (stage, fields) =>
			expectRpcOk(page, "system-event", {
				event: "chat",
				payload: liveToolLifecycle(stage, { ...base, ...fields }),
			});

		await sendLifecycle("created", { sequence: 0, providerIndex: 0 });
		const card = page.locator(`#tool-${runId}-${toolCallId}`);
		await expect(card).toBeVisible();
		await expect(card.locator(".tool-call-status")).toHaveText("preparing…");

		await sendLifecycle("input_streaming", {
			sequence: 1,
			argumentsDelta: '"sleep 10"}',
			accumulatedArguments: '{"command":"sleep 10"}',
		});
		await expect(card.locator(".tool-call-params-details .tool-call-raw-json")).toContainText("sleep 10");
		await expect(card.locator(".tool-call-status")).toHaveText("receiving parameters…");

		const commandArguments = { command: "sleep 10" };
		await sendLifecycle("input_ready", { sequence: 2, arguments: commandArguments });
		await expect(card.locator(".tool-call-status")).toHaveText("parameters ready");
		await sendLifecycle("waiting_for_execution", { sequence: 3, arguments: commandArguments });
		await expect(card.locator(".tool-call-status")).toHaveText("waiting for execution…");
		await sendLifecycle("executing", {
			sequence: 4,
			arguments: commandArguments,
			startedAtMs: 1_700_000_000_100,
		});
		await expect(card.locator(".tool-call-status")).toHaveText("running…");

		await sendLifecycle("execution_progress", {
			sequence: 5,
			arguments: commandArguments,
			elapsedMs: 9_000,
			message: "wait for result [9] sec.",
		});
		await expect(card.locator(".tool-call-result-placeholder")).toHaveText("wait for result [9] sec.");
		await expect(card.locator(".terminal-output")).toHaveCount(0);

		await sendLifecycle("execution_progress", {
			sequence: 6,
			arguments: commandArguments,
			elapsedMs: 10_000,
			message: "wait for result [10] sec.",
		});
		await expect(card.locator(".terminal-output")).toHaveCount(1);

		await sendLifecycle("result_ready", {
			sequence: 7,
			arguments: commandArguments,
			success: true,
			result: JSON.stringify({ stdout: "done\n", exit_code: 0 }),
			error: null,
		});
		await expect(card.locator(".tool-call-status")).toHaveText("result ready");
		await sendLifecycle("completed", {
			sequence: 8,
			arguments: commandArguments,
			success: true,
			result: JSON.stringify({ stdout: "done\n", exit_code: 0 }),
			error: null,
		});
		await expect(card).toHaveClass(/command-ok/);
		await expect(card.locator(".command-output")).toContainText("done");
		expect(pageErrors).toEqual([]);
	});

	test("rejected and cancelled lifecycle events render terminal cards", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await expectRpcOk(page, "chat.clear", {});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("rejected", {
				sessionKey: "main",
				runId: "run-rejected",
				toolCallId: "tool-rejected",
				toolName: "read_file",
				sequence: 4,
				arguments: { filePath: "/tmp/rejected" },
				reason: "policy denied the tool call",
				result: JSON.stringify({ error: "policy denied the tool call" }),
			}),
		});
		const rejected = page.locator("#tool-run-rejected-tool-rejected");
		await expect(rejected).toBeVisible();
		await expect(rejected.locator(".tool-call-status")).toHaveText("needs retry");
		await expect(rejected).toContainText("policy denied the tool call");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("cancelled", {
				sessionKey: "main",
				runId: "run-cancelled",
				toolCallId: "tool-cancelled",
				toolName: "overwrite_file",
				sequence: 3,
				arguments: { filePath: "/tmp/cancelled" },
				reason: "Stopped by user.",
			}),
		});
		const cancelled = page.locator("#tool-run-cancelled-tool-cancelled");
		await expect(cancelled).toBeVisible();
		await expect(cancelled.locator(".tool-call-status")).toHaveText("failed");
		await expect(cancelled).toContainText("Stopped by user.");
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
			`\n\n[Truncated — full tool result (101KB) written to file. Use the read_file tool to access the content at: ${fullOutputPath}]`;
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId,
				toolName: "execute_command",
				sequence: 8,
				arguments: { command: "df -h" },
				success: true,
				result: truncatedResult,
				error: null,
			}),
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				toolCallId,
				toolName: "execute_command",
				sequence: 2,
				arguments: { command: "df -h" },
			}),
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

	test("switch payload restores structured active reasoning parts", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:active-reasoning", messageCount: 1 },
			historyOmitted: false,
			history: [{ role: "user", content: "analyze the sources", historyIndex: 0 }],
			replying: true,
			thinkingText: [
				"**Analyzing source scope**\nReviewing the request",
				"**Checking source quality**\nComparing the evidence",
			],
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:active-reasoning");
		});

		const reasoningItems = page.locator(".msg.assistant.reasoning-stream .msg-reasoning-item");
		await expect(reasoningItems).toHaveCount(2);
		await expect(reasoningItems.nth(0)).toContainText("Analyzing source scope");
		await expect(reasoningItems.nth(1)).toContainText("Checking source quality");
		expect(pageErrors).toEqual([]);
	});

	test("switch payload restores active streamed input and backend progress", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:active-tool", messageCount: 1 },
			historyOmitted: false,
			history: [{ role: "user", content: "run two tools", historyIndex: 0 }],
			replying: true,
			activeToolInvocations: [
				lifecycleEvent("input_streaming", {
					runId: "run-switch-input",
					toolCallId: "tc-switch-input",
					toolName: "read_file",
					sequence: 1,
					argumentsDelta: '"/tmp/input"}',
					accumulatedArguments: '{"filePath":"/tmp/input"}',
				}),
				lifecycleEvent("execution_progress", {
					runId: "run-switch-progress",
					toolCallId: "tc-switch-progress",
					toolName: "execute_command",
					sequence: 5,
					arguments: { command: "sleep 10" },
					elapsedMs: 4_000,
					message: "wait for result [4] sec.",
				}),
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

		const inputCard = page.locator("#tool-run-switch-input-tc-switch-input");
		await expect(inputCard).toBeVisible();
		await expect(inputCard.locator(".tool-call-status")).toHaveText("receiving parameters…");
		await expect(inputCard.locator(".tool-call-params-details .tool-call-raw-json")).toContainText("/tmp/input");

		const progressCard = page.locator("#tool-run-switch-progress-tc-switch-progress");
		await expect(progressCard).toBeVisible();
		await expect(progressCard.locator(".tool-call-result-placeholder")).toHaveText("wait for result [4] sec.");
		expect(pageErrors).toEqual([]);
	});

	test("history replays UI-only lifecycle stages into one terminal tool card", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);
		await expectRpcOk(page, "chat.clear", {});

		const lifecycleFields = {
			runId: "run-history-lifecycle",
			toolCallId: "tc-history-lifecycle",
			toolName: "execute_command",
		};
		const commandArguments = { command: "printf history" };
		await mockRpcOkResponse(page, "sessions.switch", {
			entry: { key: "session:history-lifecycle", messageCount: 10 },
			historyOmitted: false,
			history: [
				{ role: "user", content: "run a command", historyIndex: 0 },
				{
					role: "assistant",
					content: "",
					tool_calls: [{ id: lifecycleFields.toolCallId, name: lifecycleFields.toolName }],
					historyIndex: 1,
				},
				persistedToolLifecycle("created", {
					...lifecycleFields,
					sequence: 0,
					providerIndex: 0,
					historyIndex: 2,
				}),
				persistedToolLifecycle("input_streaming", {
					...lifecycleFields,
					sequence: 1,
					argumentsDelta: '"printf history"}',
					accumulatedArguments: '{"command":"printf history"}',
					historyIndex: 3,
				}),
				persistedToolLifecycle("input_ready", {
					...lifecycleFields,
					sequence: 2,
					arguments: commandArguments,
					historyIndex: 4,
				}),
				persistedToolLifecycle("waiting_for_execution", {
					...lifecycleFields,
					sequence: 3,
					arguments: commandArguments,
					historyIndex: 5,
				}),
				persistedToolLifecycle("executing", {
					...lifecycleFields,
					sequence: 4,
					arguments: commandArguments,
					startedAtMs: 1_700_000_000_000,
					historyIndex: 6,
				}),
				persistedToolLifecycle("execution_progress", {
					...lifecycleFields,
					sequence: 5,
					arguments: commandArguments,
					elapsedMs: 1_000,
					message: "wait for result [1] sec.",
					historyIndex: 7,
				}),
				persistedToolLifecycle("result_ready", {
					...lifecycleFields,
					sequence: 6,
					arguments: commandArguments,
					success: true,
					result: JSON.stringify({ stdout: "history", exit_code: 0 }),
					error: null,
					historyIndex: 8,
				}),
				persistedToolLifecycle("completed", {
					...lifecycleFields,
					sequence: 7,
					arguments: commandArguments,
					success: true,
					result: JSON.stringify({ stdout: "history", exit_code: 0 }),
					error: null,
					historyIndex: 9,
				}),
			],
			replying: false,
		});

		await page.evaluate(async () => {
			const appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			const appUrl = new URL(appScript.src, window.location.origin);
			const prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			const sessions = await import(`${prefix}js/sessions.js`);
			sessions.switchSession("session:history-lifecycle");
		});

		const card = page.locator("#tool-run-history-lifecycle-tc-history-lifecycle");
		await expect(card).toHaveCount(1);
		await expect(card).toHaveClass(/command-ok/);
		await expect(card.locator(".command-output")).toContainText("history");
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
				persistedToolLifecycle("completed", {
					toolCallId: "tool-1",
					toolName: "execute_command",
					sequence: 8,
					arguments: {},
					success: true,
					result: null,
					error: null,
					historyIndex: 2,
				}),
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
				persistedToolLifecycle("completed", {
					toolCallId: "tool-empty-terminal",
					toolName: "execute_command",
					sequence: 8,
					arguments: {},
					success: false,
					result: null,
					error: "Tool failed.",
					created_at: 1_700_000_000_000,
					historyIndex: 2,
				}),
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

	test("final event clears stale running command status when terminal lifecycle is missed", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await page.goto("/chats/main");
		await waitForWsConnected(page);

		await expectRpcOk(page, "chat.clear", {});

		const toolCallId = "stale-command-1";
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				toolCallId,
				toolName: "execute_command",
				sequence: 2,
				arguments: { command: "df -h" },
			}),
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
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				toolCallId,
				toolName: "show_map",
				sequence: 2,
				arguments: { label: "Tartine Bakery ⭐4.7 - Open till 4PM" },
			}),
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId,
				toolName: "show_map",
				sequence: 8,
				arguments: { label: "Tartine Bakery ⭐4.7 - Open till 4PM" },
				success: true,
				result: JSON.stringify({
					label: "Tartine Bakery ⭐4.7 - Open till 4PM",
					map_links: {
						provider: "google_maps",
						url: "https://www.google.com/maps/search/?api=1&query=Tartine+Bakery&center=37.7615,-122.4241",
						google_maps: "https://www.google.com/maps/search/?api=1&query=Tartine+Bakery&center=37.7615,-122.4241",
					},
				}),
				error: null,
			}),
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
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				toolCallId,
				toolName: "show_map",
				sequence: 2,
				arguments: { label: "Breakfast spots" },
			}),
		});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey: "main",
				toolCallId,
				toolName: "show_map",
				sequence: 8,
				arguments: { label: "Breakfast spots" },
				success: true,
				result: JSON.stringify({
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
				}),
				error: null,
			}),
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

	test("reasoning stays in its assistant iteration when a tool call follows", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await navigateAndWait(page, "/chats/main");
		await waitForWsConnected(page);
		await waitForChatSessionReady(page);

		await expectRpcOk(page, "chat.clear", {});

		// 1. A persistent streaming assistant reasoning part appears.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: { sessionKey: "main", state: "thinking", runId: "run-think-tool" },
		});
		const liveSegment = page.locator(".msg.assistant.reasoning-stream");
		await expect(liveSegment.locator(".msg-reasoning.is-streaming")).toBeVisible();

		// 2. Reasoning text updates the same assistant segment.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "thinking_text",
				runId: "run-think-tool",
				text: [
					"**Analyzing recent sources**\nI need to search the web",
					"**Tracing source reliability**\nI need to compare recent news",
				],
			},
		});
		const liveReasoningItems = liveSegment.locator(".msg-reasoning-item");
		await expect(liveReasoningItems).toHaveCount(2);
		await expect(liveReasoningItems.nth(0)).toContainText("Analyzing recent sources");
		await expect(liveReasoningItems.nth(1)).toContainText("Tracing source reliability");

		// 3. Completed reasoning remains in place and collapses.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: { sessionKey: "main", state: "thinking_done", runId: "run-think-tool" },
		});
		await expect(liveSegment).toBeVisible();
		await expect(liveSegment.locator(".msg-reasoning")).not.toHaveAttribute("open", "");

		// 4. Input-ready binds the segment to the persisted assistant iteration.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				runId: "run-think-tool",
				toolCallId: "tc-ripgrep-1",
				toolName: "ripgrep",
				sequence: 2,
				arguments: { pattern: "SandboxBackend" },
				messageIndex: 999998,
				assistantMessageIndex: 999997,
				assistantMessage: {
					role: "assistant",
					content: "",
					reasoning: [
						"**Analyzing recent sources**\nI need to search the web",
						"**Tracing source reliability**\nI need to compare recent news",
					],
					tool_calls: [
						{
							id: "tc-ripgrep-1",
							function: { name: "ripgrep", arguments: '{"pattern":"SandboxBackend"}' },
						},
					],
				},
			}),
		});
		const persistedSegment = page.locator('.msg.assistant[data-history-index="999997"]');
		const persistedReasoningItems = persistedSegment.locator(".msg-reasoning-item");
		await expect(persistedReasoningItems).toHaveCount(2);
		await expect(persistedReasoningItems.nth(0)).toContainText("Analyzing recent sources");
		await expect(persistedReasoningItems.nth(1)).toContainText("Tracing source reliability");
		const toolCard = page.locator("#tool-run-think-tool-tc-ripgrep-1");
		await expect(toolCard).toBeVisible();
		await expect(toolCard.locator(".msg-reasoning")).toHaveCount(0);

		// 5. The next provider iteration owns its own reasoning disclosure.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				text: "Here are the top news stories.",
				messageIndex: 999999,
				model: "test-model",
				provider: "test-provider",
				replyMedium: "text",
				reasoning: [
					"**Analyzing final evidence**\nReviewing the tool output",
					"**Preparing the response**\nSelecting the relevant stories",
				],
			},
		});
		await expect(page.locator(".msg.assistant > .msg-reasoning")).toHaveCount(2);
		const finalReasoningItems = page.locator('.msg.assistant[data-history-index="999999"] .msg-reasoning-item');
		await expect(finalReasoningItems).toHaveCount(2);
		await expect(finalReasoningItems.nth(0)).toContainText("Analyzing final evidence");
		await expect(finalReasoningItems.nth(1)).toContainText("Preparing the response");
		expect(pageErrors).toEqual([]);
	});

	test("whitespace-only streamed assistant bubble is removed once tool input is ready/finalizes", async ({ page }) => {
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
			payload: liveToolLifecycle("input_ready", {
				sessionKey: "main",
				runId: "run-whitespace-tool",
				toolCallId: "tc-empty-1",
				toolName: "execute_command",
				sequence: 2,
				arguments: { command: "echo $FOO" },
			}),
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
