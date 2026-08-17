const { expect, test } = require("../base-test");
const { createSession, expectRpcOk, navigateAndWait, waitForWsConnected, watchPageErrors } = require("../helpers");

function liveToolLifecycle(stage, fields) {
	const { sequence = 0, emittedAtMs = 1_700_000_000_000, ...eventFields } = fields;
	return {
		...eventFields,
		state: "tool_lifecycle",
		sequence,
		emittedAtMs,
		stage,
	};
}

async function openFreshChatSession(page) {
	await navigateAndWait(page, "/");
	await waitForWsConnected(page);
	await createSession(page);
	return page.evaluate(() => window.__chelix_stores?.sessionStore?.activeSessionKey?.value || "");
}

async function startDocumentToolCall(page, sessionKey, toolCallId, filename) {
	await expect
		.poll(
			async () => {
				await expectRpcOk(page, "system-event", {
					event: "chat",
					payload: liveToolLifecycle("input_ready", {
						sessionKey,
						toolCallId,
						toolName: "send_document",
						sequence: 2,
						arguments: { path: `/tmp/${filename}` },
					}),
				});
				return page.locator(`#tool-${toolCallId} .command-status`).count();
			},
			{ timeout: 10_000 },
		)
		.toBe(1);
}

test.describe("send_document rendering", () => {
	test("renders document card with filename and download link for document_ref", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const sessionKey = await openFreshChatSession(page);

		await startDocumentToolCall(page, sessionKey, "test-doc-call", "report.pdf");

		// Simulate completed lifecycle with a document_ref result.
		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey,
				toolCallId: "test-doc-call",
				toolName: "send_document",
				sequence: 8,
				arguments: { path: "/tmp/report.pdf" },
				success: true,
				result: JSON.stringify({
					document_ref: "media/main/abc123_report.pdf",
					mime_type: "application/pdf",
					filename: "report.pdf",
					size_bytes: 12345,
				}),
				error: null,
			}),
		});

		// Verify the document card renders
		const docContainer = page.locator(".document-container").filter({ hasText: "report.pdf" });
		await expect(docContainer).toBeVisible({ timeout: 10_000 });

		// Verify filename is displayed
		const filenameEl = docContainer.locator(".document-filename");
		await expect(filenameEl).toHaveText("report.pdf");

		// Verify file size is displayed
		const sizeEl = docContainer.locator(".document-size");
		await expect(sizeEl).toHaveText("12.1 KB");

		// Verify download/open button exists and has correct href
		const downloadBtn = docContainer.locator(".document-download-btn");
		await expect(downloadBtn).toBeVisible();
		const href = await downloadBtn.getAttribute("href");
		expect(href).toContain(`/api/sessions/${encodeURIComponent(sessionKey)}/media/abc123_report.pdf`);

		// PDF should open in new tab (not trigger download)
		const target = await downloadBtn.getAttribute("target");
		expect(target).toBe("_blank");

		expect(pageErrors).toEqual([]);
	});

	test("renders document card for zip file with download attribute", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const sessionKey = await openFreshChatSession(page);

		await startDocumentToolCall(page, sessionKey, "test-zip-call", "archive.zip");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey,
				toolCallId: "test-zip-call",
				toolName: "send_document",
				sequence: 8,
				arguments: { path: "/tmp/archive.zip" },
				success: true,
				result: JSON.stringify({
					document_ref: "media/main/def456_archive.zip",
					mime_type: "application/zip",
					filename: "archive.zip",
					size_bytes: 5242880,
				}),
				error: null,
			}),
		});

		const docContainer = page.locator(".document-container").filter({ hasText: "archive.zip" });
		await expect(docContainer).toBeVisible({ timeout: 5_000 });

		const filenameEl = docContainer.locator(".document-filename");
		await expect(filenameEl).toHaveText("archive.zip");

		// Zip files should have a download attribute (not target=_blank)
		const downloadBtn = docContainer.locator(".document-download-btn");
		await expect(downloadBtn).toBeVisible();
		const downloadAttr = await downloadBtn.getAttribute("download");
		expect(downloadAttr).toBeTruthy();
		const target = await downloadBtn.getAttribute("target");
		expect(target).toBeNull();

		expect(pageErrors).toEqual([]);
	});

	test("renders document icon appropriate to file type", async ({ page }) => {
		const pageErrors = watchPageErrors(page);
		const sessionKey = await openFreshChatSession(page);

		await startDocumentToolCall(page, sessionKey, "test-csv-call", "data.csv");

		await expectRpcOk(page, "system-event", {
			event: "chat",
			payload: liveToolLifecycle("completed", {
				sessionKey,
				toolCallId: "test-csv-call",
				toolName: "send_document",
				sequence: 8,
				arguments: { path: "/tmp/data.csv" },
				success: true,
				result: JSON.stringify({
					document_ref: "media/main/ghi789_data.csv",
					mime_type: "text/csv",
					filename: "data.csv",
					size_bytes: 256,
				}),
				error: null,
			}),
		});

		const csvDoc = page.locator(".document-container").filter({ hasText: "data.csv" });
		await expect(csvDoc).toBeVisible({ timeout: 10_000 });

		// Document icon should be present
		const iconEl = csvDoc.locator(".document-icon");
		await expect(iconEl).toBeVisible();
		const iconText = await iconEl.textContent();
		expect(iconText.length).toBeGreaterThan(0);

		expect(pageErrors).toEqual([]);
	});
});
