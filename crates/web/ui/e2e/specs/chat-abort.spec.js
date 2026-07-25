const { expect, test } = require("../base-test");
const {
	expectRpcOk,
	navigateAndWait,
	sendRpcFromPage,
	waitForChatSessionReady,
	waitForWsConnected,
	watchPageErrors,
} = require("../helpers");

test.describe("Chat abort", () => {
	test.beforeEach(async ({ page }) => {
		await navigateAndWait(page, "/chats/main");
		await waitForWsConnected(page);

		await waitForChatSessionReady(page);
	});

	test("composer send button switches to stop mode while thinking", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "thinking",
				runId: "run-chat-abort-stop-button",
			},
		});

		var thinkingIndicator = page.locator("#thinkingIndicator");
		await expect(thinkingIndicator).toBeVisible({ timeout: 5_000 });

		var stopBtn = page.locator("#sendBtn");
		await expect(stopBtn).toBeVisible();
		await expect(stopBtn).toHaveAttribute("data-mode", "stop");
		await expect(stopBtn).toHaveAttribute("data-stop-session-key", "main");
		await expect(stopBtn).toHaveAttribute("title", "Stop generation");
		await expect(stopBtn.locator(".icon-stop")).toHaveCount(1);
		await expect(page.locator("#thinkingIndicator .thinking-stop-btn")).toHaveCount(0);

		expect(pageErrors).toEqual([]);
	});

	test("aborted broadcast cleans up UI state", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "thinking",
				runId: "run-chat-abort-cleanup",
			},
		});

		var thinkingIndicator = page.locator("#thinkingIndicator");
		await expect(thinkingIndicator).toBeVisible({ timeout: 5_000 });

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "aborted",
				runId: "run-chat-abort-cleanup",
			},
		});

		await expect(thinkingIndicator).toHaveCount(0, { timeout: 5_000 });
		await expect(page.locator("#sendBtn")).toHaveAttribute("data-mode", "send");
		await expect(page.locator("#sendBtn")).toHaveAttribute("title", "Send");

		expect(pageErrors).toEqual([]);
	});

	test("aborted broadcast keeps partial assistant output in UI and history cache", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "thinking",
				runId: "run-chat-abort-partial",
			},
		});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "delta",
				runId: "run-chat-abort-partial",
				text: "Partial answer",
			},
		});

		await expect(page.locator(".msg.assistant")).toContainText("Partial answer");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "aborted",
				runId: "run-chat-abort-partial",
				messageIndex: 0,
				partialMessage: {
					role: "assistant",
					content: "Partial answer",
					model: "mock-model",
					provider: "mock",
					run_id: "run-chat-abort-partial",
					created_at: Date.now(),
				},
			},
		});

		await expect(page.locator("#thinkingIndicator")).toHaveCount(0, { timeout: 5_000 });
		await expect(page.locator(".msg.assistant")).toContainText("Partial answer");

		const cachedHistory = await page.evaluate(async () => {
			var appScript = document.querySelector('script[type="module"][src*="js/app.js"]');
			if (!appScript) throw new Error("app module script not found");
			var appUrl = new URL(appScript.src, window.location.origin);
			var prefix = appUrl.href.slice(0, appUrl.href.length - "js/app.js".length);
			var cache = await import(`${prefix}js/stores/session-history-cache.js`);
			return cache.getSessionHistory("main");
		});

		expect(cachedHistory).toEqual(
			expect.arrayContaining([
				expect.objectContaining({
					role: "assistant",
					content: "Partial answer",
					run_id: "run-chat-abort-partial",
					historyIndex: 0,
				}),
			]),
		);
		expect(pageErrors).toEqual([]);
	});

	test("tool boundary recovers its canonical assistant segment and fallback final updates that segment", async ({
		page,
	}) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-abort-tool-boundary",
				messageIndex: 5,
				toolCallId: "tc-boundary-1",
				toolName: "execute_command",
				arguments: { command: "true" },
				assistantMessage: {
					role: "assistant",
					content: "Text before the tool call",
					reasoning: "I need to verify the environment first.",
					model: "mock-model",
					provider: "mock",
					inputTokens: 17,
					outputTokens: 9,
					run_id: "run-abort-tool-boundary",
					tool_calls: [{ id: "tc-boundary-1", name: "execute_command" }],
				},
			},
		});

		const toolCard = page.locator("#tool-run-abort-tool-boundary-tc-boundary-1");
		await expect(toolCard).toBeVisible({ timeout: 5_000 });
		const canonicalSegment = page.locator('.msg.assistant[data-history-index="5"]');
		await expect(canonicalSegment).toHaveCount(1);
		await expect(canonicalSegment).toContainText("Text before the tool call");
		await expect(canonicalSegment).toContainText("I need to verify the environment first.");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-abort-tool-boundary",
				messageIndex: 5,
				toolCallId: "tc-boundary-2",
				toolName: "execute_command",
				arguments: { command: "false" },
				assistantMessage: {
					role: "assistant",
					content: "Text before the tool call",
					reasoning: "I need to verify the environment first.",
					model: "mock-model",
					provider: "mock",
					tool_calls: [
						{ id: "tc-boundary-1", name: "execute_command" },
						{ id: "tc-boundary-2", name: "execute_command" },
					],
				},
			},
		});
		await expect(page.locator("#tool-run-abort-tool-boundary-tc-boundary-2")).toBeVisible({ timeout: 5_000 });
		await expect(canonicalSegment).toHaveCount(1);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "final",
				runId: "run-abort-tool-boundary",
				messageIndex: 5,
				text: "Text before the tool call",
				reasoning: "I need to verify the environment first.",
				model: "mock-model",
				provider: "mock",
				inputTokens: 17,
				outputTokens: 9,
				cacheReadTokens: 3,
				durationMs: 200,
				replyMedium: "text",
			},
		});

		await expect(canonicalSegment).toHaveCount(1);
		await expect(canonicalSegment.locator(".msg-model-footer")).toHaveCount(0);
		const metadata = page.locator('.terminal-metadata[data-history-index="5"]');
		await expect(metadata).toHaveCount(1);
		await expect(metadata).toContainText("mock / mock-model");
		await expect(metadata).toContainText("17 in (3 cached) / 9 out");
		const canonicalPrecedesCard = await page.evaluate(() => {
			var messages = document.getElementById("messages");
			if (!messages) return false;
			var card = document.getElementById("tool-run-abort-tool-boundary-tc-boundary-1");
			var segment = messages.querySelector('.msg.assistant[data-history-index="5"]');
			if (!(card && segment)) return false;
			return !!(segment.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING);
		});
		expect(canonicalPrecedesCard).toBe(true);

		expect(pageErrors).toEqual([]);
	});

	test("abort renders one standalone metadata row after a terminal tool segment", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-abort-terminal-tool-segment",
				messageIndex: 20,
				toolCallId: "tc-terminal-tool-segment",
				toolName: "execute_command",
				arguments: { command: "true" },
				assistantMessage: {
					role: "assistant",
					content: "Text before the stopped tool.",
					model: "mock-model",
					provider: "mock",
					inputTokens: 17,
					outputTokens: 9,
					cacheReadTokens: 3,
					tool_calls: [{ id: "tc-terminal-tool-segment", name: "execute_command" }],
				},
			},
		});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				runId: "run-abort-terminal-tool-segment",
				messageIndex: 21,
				toolCallId: "tc-terminal-tool-segment",
				toolName: "execute_command",
				arguments: { command: "true" },
				success: false,
				error: { detail: "Stopped by user." },
			},
		});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "aborted",
				runId: "run-abort-terminal-tool-segment",
				messageIndex: 20,
				partialMessage: {
					role: "assistant",
					content: "Text before the stopped tool.",
					model: "mock-model",
					provider: "mock",
					inputTokens: 17,
					outputTokens: 9,
					cacheReadTokens: 3,
					durationMs: 200,
					tool_calls: [{ id: "tc-terminal-tool-segment", name: "execute_command" }],
				},
			},
		});

		const segment = page.locator('.msg.assistant[data-history-index="20"]');
		await expect(segment).toHaveCount(1);
		await expect(segment.locator(".msg-model-footer")).toHaveCount(0);
		const metadata = page.locator('.terminal-metadata[data-history-index="20"]');
		await expect(metadata).toHaveCount(1);
		await expect(metadata).toContainText("mock / mock-model");
		await expect(metadata).toContainText("17 in (3 cached) / 9 out");
		expect(pageErrors).toEqual([]);
	});

	test("abort after a completed tool renders standalone metadata without an empty assistant bubble", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-abort-empty-tool-segment",
				messageIndex: 30,
				toolCallId: "tc-empty-tool-segment",
				toolName: "execute_command",
				arguments: { command: "true" },
				assistantMessage: {
					role: "assistant",
					content: "",
					model: "mock-model",
					provider: "mock",
					tool_calls: [{ id: "tc-empty-tool-segment", name: "execute_command" }],
				},
			},
		});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				runId: "run-abort-empty-tool-segment",
				messageIndex: 31,
				toolCallId: "tc-empty-tool-segment",
				toolName: "execute_command",
				arguments: { command: "true" },
				success: true,
			},
		});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "aborted",
				runId: "run-abort-empty-tool-segment",
				messageIndex: 30,
				partialMessage: {
					role: "assistant",
					content: "",
					model: "mock-model",
					provider: "mock",
					inputTokens: 17,
					outputTokens: 9,
					cacheReadTokens: 3,
					durationMs: 200,
					tool_calls: [{ id: "tc-empty-tool-segment", name: "execute_command" }],
				},
			},
		});

		const toolCard = page.locator("#tool-run-abort-empty-tool-segment-tc-empty-tool-segment");
		await expect(toolCard.locator(".msg-model-footer")).toHaveCount(0);
		const metadata = page.locator('.terminal-metadata[data-history-index="30"]');
		await expect(metadata).toHaveCount(1);
		await expect(metadata).toContainText("mock / mock-model");
		await expect(metadata).toContainText("17 in (3 cached) / 9 out");
		await expect(page.locator('.msg.assistant[data-history-index="30"]')).toHaveCount(0);
		expect(pageErrors).toEqual([]);
	});

	test("abort persists only the post-tool draft after a failed tool result", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		await expectRpcOk(page, "chat.clear", {});
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_start",
				runId: "run-abort-remainder",
				messageIndex: 10,
				toolCallId: "tc-remainder-1",
				toolName: "execute_command",
				arguments: { command: "true" },
				assistantMessage: {
					role: "assistant",
					content: "Intro segment.",
					model: "mock-model",
					provider: "mock",
					run_id: "run-abort-remainder",
					tool_calls: [
						{ id: "tc-remainder-1", name: "execute_command" },
						{ id: "tc-remainder-2", name: "execute_command" },
					],
				},
			},
		});
		await expect(page.locator("#tool-run-abort-remainder-tc-remainder-1")).toBeVisible({ timeout: 5_000 });

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				runId: "run-abort-remainder",
				messageIndex: 11,
				toolCallId: "tc-remainder-1",
				toolName: "execute_command",
				success: false,
				error: { detail: "Stopped by user." },
			},
		});
		await expect(page.locator("#tool-run-abort-remainder-tc-remainder-1")).toHaveClass(/command-err/);
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "tool_call_end",
				runId: "run-abort-remainder",
				messageIndex: 12,
				toolCallId: "tc-remainder-2",
				toolName: "execute_command",
				arguments: { command: "false" },
				success: false,
				error: { detail: "Stopped by user." },
			},
		});
		await expect(page.locator("#tool-run-abort-remainder-tc-remainder-2")).toHaveClass(/command-err/);
		await expect(page.getByText("Stopped by user.", { exact: true })).toHaveCount(2);

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: {
				sessionKey: "main",
				state: "aborted",
				runId: "run-abort-remainder",
				messageIndex: 13,
				partialMessage: {
					role: "assistant",
					content: "Post-tool draft.",
					model: "mock-model",
					provider: "mock",
					run_id: "run-abort-remainder",
					created_at: Date.now(),
				},
			},
		});

		const preToolSegment = page.locator('.msg.assistant[data-history-index="10"]');
		const postToolSegment = page.locator('.msg.assistant[data-history-index="13"]');
		await expect(preToolSegment).toHaveCount(1);
		await expect(postToolSegment).toHaveCount(1);
		await expect(preToolSegment).toContainText("Intro segment.");
		await expect(postToolSegment).toContainText("Post-tool draft.");
		await expect(postToolSegment).not.toContainText("Intro segment.");
		const postToolFollowsCard = await page.evaluate(() => {
			var messages = document.getElementById("messages");
			if (!messages) return false;
			var card = document.getElementById("tool-run-abort-remainder-tc-remainder-1");
			var segment = messages.querySelector('.msg.assistant[data-history-index="13"]');
			if (!(card && segment)) return false;
			return !!(card.compareDocumentPosition(segment) & Node.DOCUMENT_POSITION_FOLLOWING);
		});
		expect(postToolFollowsCard).toBe(true);

		expect(pageErrors).toEqual([]);
	});

	test("chat.peek RPC returns result", async ({ page }) => {
		const pageErrors = watchPageErrors(page);

		// Peek at an idle session — should return { active: false }.
		var peekRes = await sendRpcFromPage(page, "chat.peek", { sessionKey: "main" });
		expect(peekRes).toBeTruthy();
		// It's fine if it returns ok: false due to no active run.
		// The important thing is that the RPC is registered and doesn't crash.
		if (peekRes?.active !== undefined) {
			expect(peekRes.active).toBe(false);
		}

		expect(pageErrors).toEqual([]);
	});
});
