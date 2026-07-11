// ── Tool call card renderer ──────────────────────────────────

import { renderCommand } from "./code-highlight";
import { renderDocument, renderMapLinks, renderMapPointGroups, renderScreenshot, toolCallSummary } from "./helpers";
import type { ToolError, ToolResult } from "./types/ws-events";

export type ToolCardStatus = "running" | "success" | "error" | "retry";

export interface ToolCardOptions {
	id?: string;
	toolCallId?: string;
	assistantHistoryIndex?: number;
	toolName?: string;
	arguments?: unknown;
	executionMode?: string;
	status?: ToolCardStatus;
	expanded?: boolean;
}

export interface ToolResultRenderOptions {
	sessionKey?: string;
	screenshotMode?: "inline-base64" | "media";
}

const STATUS_LABELS: Record<ToolCardStatus, string> = {
	running: "running…",
	success: "completed",
	error: "failed",
	retry: "needs retry",
};

const TRUNCATED_RESULT_MARKER = "\n\n[Truncated —";
const TEXT_RESULT_FIELDS = ["stdout", "output", "stderr"] as const;

function stringifyValue(value: unknown): string {
	if (value === undefined) return "{}";
	if (typeof value === "string") {
		try {
			return JSON.stringify(JSON.parse(value), null, 2);
		} catch (_err) {
			return value;
		}
	}
	try {
		const json = JSON.stringify(value ?? {}, null, 2);
		return json || String(value ?? "");
	} catch (_err) {
		return String(value ?? "");
	}
}

function compactOneLine(value: unknown): string {
	return stringifyValue(value).replace(/\s+/g, " ").trim();
}

function buildToolSummary(toolName: string | undefined, args: unknown, executionMode?: string): string {
	const specialized = toolCallSummary(
		toolName,
		args && typeof args === "object" ? (args as Parameters<typeof toolCallSummary>[1]) : undefined,
		executionMode,
	);
	const normalizedName = toolName || "tool";
	if (specialized && specialized !== normalizedName && specialized !== "tool") return specialized;
	const compactArgs = compactOneLine(args);
	if (!compactArgs || compactArgs === "{}") return normalizedName;
	return `${normalizedName} ${compactArgs}`;
}

export function isCommandToolName(toolName: string | undefined): boolean {
	return toolName === "execute_command";
}

function makeLabeledPre(label: string, text: string, className: string): HTMLElement {
	const wrap = document.createElement("div");
	wrap.className = "tool-call-output-block";

	const labelEl = document.createElement("div");
	labelEl.className = "tool-call-output-label";
	labelEl.textContent = label;
	wrap.appendChild(labelEl);

	const pre = document.createElement("pre");
	pre.className = className;
	pre.textContent = text;
	pre.setAttribute("data-tool-stream", label.toLowerCase());
	wrap.appendChild(pre);

	return wrap;
}

function getResultContent(card: HTMLElement): HTMLElement {
	const existing = card.querySelector("[data-tool-result-content]") as HTMLElement | null;
	if (existing) return existing;
	return card;
}

function getStatusEl(card: HTMLElement): HTMLElement | null {
	return card.querySelector(".command-status") as HTMLElement | null;
}

function appendRawPayload(
	container: HTMLElement,
	label: string,
	payload: unknown,
	options: { open?: boolean; className?: string } = {},
): void {
	const raw = document.createElement("details");
	raw.className = options.className ? `tool-call-raw ${options.className}` : "tool-call-raw";
	raw.open = options.open === true;

	const summary = document.createElement("summary");
	summary.textContent = label;
	raw.appendChild(summary);

	const pre = document.createElement("pre");
	pre.className = "tool-call-json tool-call-raw-json";
	pre.textContent = stringifyValue(payload);
	raw.appendChild(pre);

	container.appendChild(raw);
}

function resolveScreenshotSrc(screenshot: string, options: ToolResultRenderOptions): string {
	if (screenshot.startsWith("data:")) return screenshot;
	if (options.screenshotMode === "media") {
		const filename = screenshot.split("/").pop() || "";
		const sessionKey = options.sessionKey || "main";
		return `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(filename)}`;
	}
	return `data:image/png;base64,${screenshot}`;
}

function resultExitCode(result: ToolResult): number | undefined {
	const raw = result.exit_code ?? result.exitCode;
	return typeof raw === "number" && Number.isFinite(raw) ? raw : undefined;
}

function decodeJsonStringPrefix(value: string): string {
	let decoded = "";
	for (let index = 0; index < value.length; index += 1) {
		const char = value[index];
		if (char === '"') break;
		if (char !== "\\") {
			decoded += char;
			continue;
		}
		index += 1;
		if (index >= value.length) break;
		const escaped = value[index];
		if (escaped === "n") decoded += "\n";
		else if (escaped === "r") decoded += "\r";
		else if (escaped === "t") decoded += "\t";
		else if (escaped === "b") decoded += "\b";
		else if (escaped === "f") decoded += "\f";
		else if (escaped === '"' || escaped === "\\" || escaped === "/") decoded += escaped;
		else if (escaped === "u") {
			const hex = value.slice(index + 1, index + 5);
			if (!/^[0-9a-fA-F]{4}$/.test(hex)) break;
			decoded += String.fromCharCode(Number.parseInt(hex, 16));
			index += 4;
		}
	}
	return decoded;
}

function extractTruncatedTextField(jsonPrefix: string, field: (typeof TEXT_RESULT_FIELDS)[number]): string | undefined {
	const fieldStart = jsonPrefix.indexOf(`"${field}":"`);
	if (fieldStart < 0) return undefined;
	const valueStart = fieldStart + field.length + 4;
	return decodeJsonStringPrefix(jsonPrefix.slice(valueStart));
}

export function normalizeToolResult(result: ToolResult | string): ToolResult {
	if (typeof result !== "string") return result;
	try {
		const parsed: unknown = JSON.parse(result);
		if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) return parsed as ToolResult;
	} catch (_err) {
		// Truncated canonical JSON is decoded below.
	}

	const markerIndex = result.indexOf(TRUNCATED_RESULT_MARKER);
	if (markerIndex < 0) return { output: result };
	const jsonPrefix = result.slice(0, markerIndex);
	const pointer = result.slice(markerIndex + 2);
	const normalized: ToolResult = {};
	for (const field of TEXT_RESULT_FIELDS) {
		const text = extractTruncatedTextField(jsonPrefix, field);
		if (text !== undefined) normalized[field] = text;
	}
	const pointerField =
		normalized.output !== undefined ? "output" : normalized.stdout !== undefined ? "stdout" : undefined;
	if (pointerField) {
		normalized[pointerField] = `${normalized[pointerField]}\n\n${pointer}`;
		return normalized;
	}
	return { output: `${jsonPrefix}\n\n${pointer}` };
}

export function createToolCallCard(options: ToolCardOptions): HTMLElement {
	const toolName = options.toolName || "tool";
	const status = options.status || "running";
	const expanded = options.expanded ?? (status === "running" || isCommandToolName(toolName));

	const card = document.createElement("div");
	card.className = "msg command-card tool-call-card";
	if (options.id) card.id = options.id;
	if (options.toolCallId) card.dataset.toolCallId = options.toolCallId;
	if (Number.isInteger(options.assistantHistoryIndex)) {
		card.dataset.assistantHistoryIndex = String(options.assistantHistoryIndex);
	}
	card.setAttribute("data-tool-name", toolName);

	const header = document.createElement("div");
	header.className = "tool-call-header";

	const toggle = document.createElement("button");
	toggle.type = "button";
	toggle.className = "tool-call-toggle";
	toggle.setAttribute("aria-expanded", String(expanded));

	const metaRow = document.createElement("span");
	metaRow.className = "tool-call-meta-row";

	const chevron = document.createElement("span");
	chevron.className = "tool-call-chevron";
	chevron.setAttribute("aria-hidden", "true");
	chevron.textContent = expanded ? "⌄" : "›";
	metaRow.appendChild(chevron);

	const nameEl = document.createElement("span");
	nameEl.className = "tool-call-name";
	nameEl.textContent = toolName;
	metaRow.appendChild(nameEl);

	const statusEl = document.createElement("span");
	statusEl.className = "command-status tool-call-status";
	metaRow.appendChild(statusEl);

	if (options.executionMode) {
		const modeEl = document.createElement("span");
		modeEl.className = "tool-call-mode";
		modeEl.textContent = options.executionMode;
		metaRow.appendChild(modeEl);
	}

	toggle.appendChild(metaRow);
	header.appendChild(toggle);

	const summaryEl = document.createElement("span");
	summaryEl.className = "command-prompt tool-call-summary";
	renderCommand(summaryEl, buildToolSummary(toolName, options.arguments, options.executionMode));
	header.appendChild(summaryEl);

	card.appendChild(header);

	const details = document.createElement("div");
	details.className = "tool-call-details";
	details.hidden = !expanded;
	if (options.id) {
		details.id = `${options.id}-details`;
		toggle.setAttribute("aria-controls", details.id);
	}

	appendRawPayload(details, "Parameters", options.arguments, {
		open: !isCommandToolName(toolName),
		className: "tool-call-params-details",
	});

	const resultSection = document.createElement("section");
	resultSection.className = "tool-call-section tool-call-result-section";

	const resultTitle = document.createElement("div");
	resultTitle.className = "tool-call-section-title";
	resultTitle.textContent = "Result";
	resultSection.appendChild(resultTitle);

	const resultContent = document.createElement("div");
	resultContent.className = "tool-call-result-content";
	resultContent.setAttribute("data-tool-result-content", "");
	const placeholder = document.createElement("div");
	placeholder.className = "tool-call-result-placeholder";
	placeholder.textContent = status === "running" ? "Waiting for tool result…" : "No result payload.";
	resultContent.appendChild(placeholder);
	resultSection.appendChild(resultContent);
	details.appendChild(resultSection);

	card.appendChild(details);

	toggle.addEventListener("click", () => {
		setToolCardExpanded(card, !isToolCardExpanded(card));
	});

	setToolCardStatus(card, status);
	setToolCardExpanded(card, expanded);
	return card;
}

export function toolCallIds(toolCalls: unknown): string[] {
	if (!Array.isArray(toolCalls)) return [];
	const ids: string[] = [];
	const seen = new Set<string>();
	for (const toolCall of toolCalls) {
		if (!(toolCall && typeof toolCall === "object" && "id" in toolCall)) continue;
		const id = (toolCall as { id?: unknown }).id;
		if (typeof id !== "string" || !id || seen.has(id)) continue;
		seen.add(id);
		ids.push(id);
	}
	return ids;
}

export function resolveToolBatchEnd(toolCallIdsForBatch: readonly string[]): HTMLElement | null {
	if (toolCallIdsForBatch.length === 0) return null;
	const cardsByToolCallId = new Map<string, HTMLElement>();
	for (const card of document.querySelectorAll<HTMLElement>(".tool-call-card[data-tool-call-id]")) {
		const toolCallId = card.dataset.toolCallId;
		if (toolCallId) cardsByToolCallId.set(toolCallId, card);
	}
	if (!toolCallIdsForBatch.every((toolCallId) => cardsByToolCallId.has(toolCallId))) return null;
	return toolCallIdsForBatch
		.map((toolCallId) => cardsByToolCallId.get(toolCallId))
		.reduce<HTMLElement | null>((last, card) => {
			if (!card) return last;
			if (!last || last.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING) return card;
			return last;
		}, null);
}

export function resolveAssistantTurnEnd(
	historyIndex: number | undefined,
	assistantEl: HTMLElement | null,
): HTMLElement | null {
	if (!Number.isInteger(historyIndex)) return assistantEl;
	let lastToolCard: HTMLElement | null = null;
	for (const card of document.querySelectorAll<HTMLElement>(
		`.tool-call-card[data-assistant-history-index="${historyIndex}"]`,
	)) {
		if (!lastToolCard || lastToolCard.compareDocumentPosition(card) & Node.DOCUMENT_POSITION_FOLLOWING) {
			lastToolCard = card;
		}
	}
	return lastToolCard || assistantEl;
}

export function getToolCardDetailsContainer(card: HTMLElement): HTMLElement {
	return (card.querySelector(".tool-call-details") as HTMLElement | null) || card;
}

export function isToolCardExpanded(card: HTMLElement): boolean {
	const details = card.querySelector(".tool-call-details") as HTMLElement | null;
	return details ? !details.hidden : !card.classList.contains("is-collapsed");
}

export function setToolCardExpanded(card: HTMLElement, expanded: boolean): void {
	card.classList.toggle("is-collapsed", !expanded);
	const details = card.querySelector(".tool-call-details") as HTMLElement | null;
	if (details) details.hidden = !expanded;
	const toggle = card.querySelector(".tool-call-toggle") as HTMLElement | null;
	if (toggle) toggle.setAttribute("aria-expanded", String(expanded));
	const chevron = card.querySelector(".tool-call-chevron") as HTMLElement | null;
	if (chevron) chevron.textContent = expanded ? "⌄" : "›";
}

export function setToolCardStatus(card: HTMLElement, status: ToolCardStatus, label?: string): void {
	card.classList.remove("running", "command-ok", "command-err", "command-retry");
	if (status === "running") card.classList.add("running");
	if (status === "success") card.classList.add("command-ok");
	if (status === "error") card.classList.add("command-err");
	if (status === "retry") card.classList.add("command-retry");
	card.setAttribute("data-tool-status", status);
	const statusEl = getStatusEl(card);
	if (statusEl) statusEl.textContent = label || STATUS_LABELS[status];
}

export function appendToolOutputChunk(card: HTMLElement, stream: "stdout" | "stderr", chunk: string): void {
	if (!chunk) return;
	const content = getResultContent(card);
	const placeholder = content.querySelector(".tool-call-result-placeholder");
	if (placeholder) placeholder.remove();
	let pre = content.querySelector(`pre[data-tool-stream="${stream}"]`) as HTMLPreElement | null;
	if (!pre) {
		const block = makeLabeledPre(
			stream,
			"",
			stream === "stderr" ? "command-output command-stderr tool-call-output" : "command-output tool-call-output",
		);
		content.appendChild(block);
		pre = block.querySelector("pre") as HTMLPreElement | null;
	}
	if (pre) pre.textContent = `${pre.textContent || ""}${chunk}`;
}

export function renderToolCardResult(
	card: HTMLElement,
	resultValue: ToolResult | string,
	options: ToolResultRenderOptions = {},
): void {
	const result = normalizeToolResult(resultValue);
	const content = getResultContent(card);
	content.textContent = "";

	let renderedVisibleResult = false;
	const stdout = (result.stdout || "").replace(/\n+$/, "");
	if (stdout) {
		content.appendChild(makeLabeledPre("stdout", stdout, "command-output tool-call-output"));
		renderedVisibleResult = true;
	}

	const output = (result.output || "").replace(/\n+$/, "");
	if (output) {
		content.appendChild(makeLabeledPre("output", output, "command-output tool-call-output"));
		renderedVisibleResult = true;
	}

	const stderr = (result.stderr || "").replace(/\n+$/, "");
	if (stderr) {
		content.appendChild(makeLabeledPre("stderr", stderr, "command-output command-stderr tool-call-output"));
		renderedVisibleResult = true;
	}

	const exitCode = resultExitCode(result);
	if (exitCode !== undefined && exitCode !== 0) {
		const codeEl = document.createElement("div");
		codeEl.className = "command-exit command-exit-error";
		codeEl.textContent = `exit ${exitCode}`;
		content.appendChild(codeEl);
		renderedVisibleResult = true;
	}

	if (!renderedVisibleResult && result.message) {
		const messageEl = document.createElement("div");
		messageEl.className = "tool-call-result-placeholder";
		messageEl.textContent = result.message;
		content.appendChild(messageEl);
		renderedVisibleResult = true;
	}

	if (result.screenshot) {
		renderScreenshot(content, resolveScreenshotSrc(result.screenshot, options), result.screenshot_scale || 1);
		renderedVisibleResult = true;
	}

	if (result.document_ref) {
		const docStoredName = result.document_ref.split("/").pop() || "";
		const docDisplayName = result.filename || docStoredName;
		const sessionKey = options.sessionKey || "main";
		const docMediaSrc = `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(docStoredName)}`;
		renderDocument(content, docMediaSrc, docDisplayName, result.mime_type, result.size_bytes);
		renderedVisibleResult = true;
	}

	const renderedPointGroups = renderMapPointGroups(content, result.points, result.label);
	if (renderedPointGroups) renderedVisibleResult = true;
	if (!renderedPointGroups && result.map_links) {
		renderMapLinks(content, result.map_links, result.label);
		renderedVisibleResult = true;
	}

	if (!renderedVisibleResult) {
		const empty = document.createElement("div");
		empty.className = "tool-call-result-placeholder";
		empty.textContent = "No textual output.";
		content.appendChild(empty);
	}

	appendRawPayload(content, "Raw result payload", result, { open: !renderedVisibleResult });
}

export function appendToolCardError(card: HTMLElement, error: ToolError | string | undefined, retry = false): void {
	const content = getResultContent(card);

	const message = typeof error === "string" ? error : error?.detail || error?.message || "Tool call failed.";
	const errMsg = document.createElement("div");
	errMsg.className = retry ? "command-retry-detail" : "command-error-detail";
	errMsg.textContent = message;
	content.appendChild(errMsg);

	if (error && typeof error !== "string") appendRawPayload(content, "Raw error payload", error);
}

export function renderToolCardError(card: HTMLElement, error: ToolError | string | undefined, retry = false): void {
	const content = getResultContent(card);
	content.textContent = "";
	appendToolCardError(card, error, retry);
}
