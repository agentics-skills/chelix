// ── Tool call card renderer ──────────────────────────────────

import { renderCommand } from "./code-highlight";
import { renderDocument, renderMapLinks, renderMapPointGroups, renderScreenshot, toolCallSummary } from "./helpers";
import type { ContextBudgetMetadata, ToolError, ToolResult } from "./types/ws-events";

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
	if (value === undefined) return "";
	if (typeof value === "string") {
		try {
			return JSON.stringify(JSON.parse(value), null, 2);
		} catch (_err) {
			return value;
		}
	}
	try {
		const json = JSON.stringify(value, null, 2);
		return json ?? String(value);
	} catch (_err) {
		return String(value);
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

function makeOutputBlock(stream: "stdout" | "output" | "stderr", text: string, className: string): HTMLElement {
	const wrap = document.createElement("div");
	wrap.className = "tool-call-output-block";

	const pre = document.createElement("pre");
	pre.className = className;
	pre.textContent = text;
	pre.setAttribute("data-tool-stream", stream);
	wrap.appendChild(pre);

	return wrap;
}

function getSectionContent(card: HTMLElement, section: "result" | "output"): HTMLElement {
	const content = card.querySelector<HTMLElement>(`[data-tool-${section}-content]`);
	if (!content) throw new Error(`tool card ${section} content is unavailable`);
	return content;
}

function setSectionTitleVisible(card: HTMLElement, section: "result" | "output", visible: boolean): void {
	const title = card.querySelector<HTMLElement>(`.tool-call-${section}-section > .tool-call-section-title`);
	if (!title) throw new Error(`tool card ${section} title is unavailable`);
	title.hidden = !visible;
}

function getResultContent(card: HTMLElement): HTMLElement {
	return getSectionContent(card, "result");
}

function getOutputContent(card: HTMLElement): HTMLElement {
	return getSectionContent(card, "output");
}

export function setToolCardOutputVisible(card: HTMLElement, visible: boolean): void {
	setSectionTitleVisible(card, "output", visible);
}

function getStatusEl(card: HTMLElement): HTMLElement | null {
	return card.querySelector(".command-status") as HTMLElement | null;
}

function appendRawPayload(
	container: HTMLElement,
	label: string,
	payload: unknown,
	options: { open?: boolean; className?: string } = {},
): HTMLDetailsElement {
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
	return raw;
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

const SIMPLE_JSON_ESCAPES: Record<string, string> = {
	n: "\n",
	r: "\r",
	t: "\t",
	b: "\b",
	f: "\f",
	'"': '"',
	"\\": "\\",
	"/": "/",
};

interface DecodedJsonEscape {
	text: string;
	endIndex: number;
}

function decodeJsonEscape(value: string, escapeIndex: number): DecodedJsonEscape | null {
	const escaped = value[escapeIndex];
	if (escaped === undefined) return null;
	if (escaped !== "u") {
		return { text: SIMPLE_JSON_ESCAPES[escaped] ?? "", endIndex: escapeIndex };
	}
	const hex = value.slice(escapeIndex + 1, escapeIndex + 5);
	if (!/^[0-9a-fA-F]{4}$/.test(hex)) return null;
	return { text: String.fromCharCode(Number.parseInt(hex, 16)), endIndex: escapeIndex + 4 };
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
		const decodedEscape = decodeJsonEscape(value, index + 1);
		if (!decodedEscape) break;
		decoded += decodedEscape.text;
		index = decodedEscape.endIndex;
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

	const resultSection = document.createElement("section");
	resultSection.className = "tool-call-section tool-call-result-section";

	const resultTitle = document.createElement("div");
	resultTitle.className = "tool-call-section-title";
	resultTitle.textContent = "Result";
	resultTitle.hidden = true;
	resultSection.appendChild(resultTitle);

	const resultContent = document.createElement("div");
	resultContent.className = "tool-call-result-content";
	resultContent.setAttribute("data-tool-result-content", "");
	resultSection.appendChild(resultContent);
	details.appendChild(resultSection);

	const outputSection = document.createElement("section");
	outputSection.className = "tool-call-section tool-call-output-section";

	const outputTitle = document.createElement("div");
	outputTitle.className = "tool-call-section-title";
	outputTitle.textContent = "Output";
	outputTitle.hidden = true;
	outputSection.appendChild(outputTitle);

	const outputContent = document.createElement("div");
	outputContent.className = "tool-call-result-content tool-call-output-content";
	outputContent.setAttribute("data-tool-output-content", "");
	outputSection.appendChild(outputContent);
	details.appendChild(outputSection);

	// The summary line above already carries the call in readable form, so the
	// raw arguments start collapsed for every tool.
	appendRawPayload(details, "Parameters", options.arguments, {
		className: "tool-call-params-details",
	});

	card.appendChild(details);

	toggle.addEventListener("click", () => {
		setToolCardExpanded(card, !isToolCardExpanded(card));
	});

	setToolCardStatus(card, status);
	setToolCardExpanded(card, expanded);
	return card;
}

export function updateToolCardParameters(card: HTMLElement, argumentsValue: unknown, executionMode?: string): void {
	const toolName = card.dataset.toolName || "tool";
	const parameters = card.querySelector<HTMLElement>(".tool-call-params-details .tool-call-raw-json");
	if (!parameters) throw new Error("tool card Parameters content is unavailable");
	parameters.textContent = stringifyValue(argumentsValue);
	const summary = card.querySelector<HTMLElement>(".tool-call-summary");
	if (!summary) throw new Error("tool card summary is unavailable");
	renderCommand(summary, buildToolSummary(toolName, argumentsValue, executionMode));
}

export function setToolCardProgress(card: HTMLElement, message: string): void {
	setToolCardStatus(card, "running", message || STATUS_LABELS.running);
	getResultContent(card).textContent = "";
	setSectionTitleVisible(card, "result", false);
	const details = getToolCardDetailsContainer(card);
	details.querySelector(".tool-call-result-payload-details")?.remove();
	details.querySelector(".tool-call-context-budget-details")?.remove();
}

export function renderToolCardProgress(card: HTMLElement, message: string | null): void {
	const content = getOutputContent(card);
	content.textContent = "";
	if (message?.trim()) {
		const output = document.createElement("div");
		output.className = "tool-call-result-placeholder";
		output.textContent = message;
		content.appendChild(output);
	}
	setToolCardOutputVisible(card, content.childElementCount > 0);
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
	const content = getOutputContent(card);
	let pre = content.querySelector(`pre[data-tool-stream="${stream}"]`) as HTMLPreElement | null;
	if (!(pre || chunk.trim())) return;

	content.querySelector(".tool-call-result-placeholder")?.remove();
	if (!pre) {
		const block = makeOutputBlock(
			stream,
			"",
			stream === "stderr" ? "command-output command-stderr tool-call-output" : "command-output tool-call-output",
		);
		content.appendChild(block);
		pre = block.querySelector("pre") as HTMLPreElement | null;
	}
	if (pre) pre.textContent = `${pre.textContent || ""}${chunk}`;
	setToolCardOutputVisible(card, content.childElementCount > 0);
}

interface ToolResultStream {
	field: "stdout" | "output" | "stderr";
	className: string;
}

const TOOL_RESULT_STREAMS: ToolResultStream[] = [
	{ field: "stdout", className: "command-output tool-call-output" },
	{ field: "output", className: "command-output tool-call-output" },
	{ field: "stderr", className: "command-output command-stderr tool-call-output" },
];

function renderToolResultStreams(content: HTMLElement, result: ToolResult): void {
	for (const stream of TOOL_RESULT_STREAMS) {
		const text = (result[stream.field] || "").replace(/\n+$/, "");
		if (!text.trim()) continue;
		content.appendChild(makeOutputBlock(stream.field, text, stream.className));
	}
}

function renderToolExitCode(content: HTMLElement, result: ToolResult): boolean {
	const exitCode = resultExitCode(result);
	if (exitCode === undefined || exitCode === 0) return false;
	const codeEl = document.createElement("div");
	codeEl.className = "command-exit command-exit-error";
	codeEl.textContent = `exit ${exitCode}`;
	content.appendChild(codeEl);
	return true;
}

function renderToolMessage(content: HTMLElement, result: ToolResult): void {
	if (!result.message?.trim()) return;
	const messageEl = document.createElement("div");
	messageEl.className = "tool-call-result-placeholder";
	messageEl.textContent = result.message;
	content.appendChild(messageEl);
}

function renderToolScreenshot(content: HTMLElement, result: ToolResult, options: ToolResultRenderOptions): boolean {
	if (!result.screenshot) return false;
	renderScreenshot(content, resolveScreenshotSrc(result.screenshot, options), result.screenshot_scale || 1);
	return true;
}

function renderToolDocument(content: HTMLElement, result: ToolResult, options: ToolResultRenderOptions): boolean {
	if (!result.document_ref) return false;
	const storedName = result.document_ref.split("/").pop() || "";
	const displayName = result.filename || storedName;
	const sessionKey = options.sessionKey || "main";
	const mediaSrc = `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(storedName)}`;
	renderDocument(content, mediaSrc, displayName, result.mime_type, result.size_bytes);
	return true;
}

function renderToolMap(content: HTMLElement, result: ToolResult): boolean {
	if (renderMapPointGroups(content, result.points, result.label)) return true;
	if (!result.map_links) return false;
	renderMapLinks(content, result.map_links, result.label);
	return true;
}

export function renderToolCardResult(
	card: HTMLElement,
	resultValue: ToolResult | string | null,
	options: ToolResultRenderOptions = {},
): void {
	const details = getToolCardDetailsContainer(card);
	const parameters = details.querySelector<HTMLElement>(".tool-call-params-details");
	if (!parameters) throw new Error("tool card Parameters disclosure is unavailable");

	const result = resultValue === null ? null : normalizeToolResult(resultValue);
	const resultContent = getResultContent(card);
	const outputContent = getOutputContent(card);
	resultContent.textContent = "";
	outputContent.textContent = "";

	if (result) {
		renderToolResultStreams(outputContent, result);
		renderToolExitCode(resultContent, result);
		renderToolMessage(resultContent, result);
		renderToolScreenshot(resultContent, result, options);
		renderToolDocument(resultContent, result, options);
		renderToolMap(resultContent, result);
	}
	setSectionTitleVisible(card, "result", resultContent.childElementCount > 0);
	setToolCardOutputVisible(card, outputContent.childElementCount > 0);

	details.querySelector(".tool-call-result-payload-details")?.remove();
	if (!result) return;

	const rawPayload = appendRawPayload(details, "Raw result payload", result, {
		className: "tool-call-result-payload-details",
	});
	parameters.after(rawPayload);
}

export function appendToolCardContextBudget(card: HTMLElement, contextBudget: ContextBudgetMetadata | undefined): void {
	const details = getToolCardDetailsContainer(card);
	details.querySelector(".tool-call-context-budget-details")?.remove();
	if (!contextBudget) return;

	const anchor =
		details.querySelector<HTMLElement>(".tool-call-result-payload-details") ||
		details.querySelector<HTMLElement>(".tool-call-params-details");
	if (!anchor) throw new Error("tool card diagnostic disclosures are unavailable");

	const disclosure = appendRawPayload(details, "Context budget", contextBudget, {
		className: "tool-call-context-budget-details",
	});
	anchor.after(disclosure);
}

export function appendToolCardError(card: HTMLElement, error: ToolError | string | undefined, retry = false): void {
	const content = getResultContent(card);
	const message = typeof error === "string" ? error : error?.detail || error?.message;
	if (!message?.trim()) throw new Error("tool card error message is unavailable");

	const errMsg = document.createElement("div");
	errMsg.className = retry ? "command-retry-detail" : "command-error-detail";
	errMsg.textContent = message;
	content.appendChild(errMsg);

	if (error && typeof error !== "string") appendRawPayload(content, "Raw error payload", error);
	setSectionTitleVisible(card, "result", true);
}

export function renderToolCardError(card: HTMLElement, error: ToolError | string | undefined, retry = false): void {
	const resultContent = getResultContent(card);
	resultContent.textContent = "";
	getOutputContent(card).textContent = "";
	setToolCardOutputVisible(card, false);
	getToolCardDetailsContainer(card).querySelector(".tool-call-result-payload-details")?.remove();
	appendToolCardError(card, error, retry);
}
