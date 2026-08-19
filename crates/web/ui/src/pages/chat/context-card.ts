// ── Context card rendering ───────────────────────────────────

import { chatInsertionTarget, smartScrollToBottom } from "../../chat-ui";
import { formatBytes, formatTokens } from "../../helpers";
import * as S from "../../state";
import type { ChatContextPayload, ChatContextTokenUsage, PromptMemoryData } from "../../types/chat";
import { slashInjectStyles } from "./slash-commands";

// ── Types ────────────────────────────────────────────────────

/** Persisted checkpoint message fields used by the checkpoint card. */
export interface CheckpointCardData {
	summary?: string;
	model?: string;
	provider?: string;
	inputTokens?: number;
	outputTokens?: number;
	messagesSummarized?: number;
	created_at?: number;
}

// ── DOM helpers ──────────────────────────────────────────────

export function ctxEl(tag: string, cls: string, text?: string): HTMLElement {
	const el = document.createElement(tag);
	if (cls) el.className = cls;
	if (text !== undefined) el.textContent = text;
	return el;
}

export function ctxRow(label: string, value: string, mono?: boolean): HTMLElement {
	const row = ctxEl("div", "ctx-row");
	row.appendChild(ctxEl("span", "ctx-label", label));
	row.appendChild(ctxEl("span", `ctx-value${mono ? " mono" : ""}`, value));
	return row;
}

export function ctxSection(title: string): HTMLElement {
	const sec = ctxEl("div", "ctx-section");
	sec.appendChild(ctxEl("div", "ctx-section-title", title));
	return sec;
}

// ── Checkpoint card ──────────────────────────────────────────

/**
 * Render the persistent conversation-summarization checkpoint card.
 *
 * Used both by history rendering (`role === "checkpoint"` messages) and by
 * live `compact` / `auto_compact` broadcasts, so the card looks identical
 * in both paths. Appends to the chat message box and returns the element.
 */
export function renderCheckpointCard(data: CheckpointCardData): HTMLElement | null {
	const target = chatInsertionTarget();
	if (!target) return null;
	slashInjectStyles();
	const card = ctxEl("div", "ctx-card checkpoint-card");
	const header = ctxEl("div", "ctx-header");
	const icon = document.createElement("span");
	icon.className = "icon icon-compress";
	header.appendChild(icon);
	header.appendChild(ctxEl("span", "ctx-header-title", "Conversation summarized"));
	card.appendChild(header);
	const sec = ctxSection("Checkpoint");
	if (data.model) sec.appendChild(ctxRow("Model", data.model));
	const inputTokens = Number(data.inputTokens || 0);
	const outputTokens = Number(data.outputTokens || 0);
	const totalTokens = inputTokens + outputTokens;
	if (totalTokens > 0) {
		sec.appendChild(
			ctxRow(
				"Tokens used",
				`${formatTokens(totalTokens)} (${formatTokens(inputTokens)} in + ${formatTokens(outputTokens)} out)`,
			),
		);
	}
	if (data.messagesSummarized) sec.appendChild(ctxRow("Messages", `${data.messagesSummarized} summarized`));
	sec.appendChild(ctxRow("Status", "Context restarts from this checkpoint"));
	card.appendChild(sec);
	if (data.summary) {
		const details = document.createElement("details");
		details.className = "ctx-section checkpoint-summary";
		const summaryToggle = document.createElement("summary");
		summaryToggle.className = "ctx-section-title";
		summaryToggle.textContent = "View summary";
		details.appendChild(summaryToggle);
		const body = ctxEl("div", "ctx-value checkpoint-summary-text", data.summary);
		details.appendChild(body);
		card.appendChild(details);
	}
	target.appendChild(card);
	return card;
}

// ── Prompt memory helpers ────────────────────────────────────

export function formatPromptMemoryMode(mode: string | undefined): string {
	if (mode === "frozen-at-session-start") return "Frozen at session start";
	if (mode === "live-reload") return "Live reload";
	return mode || "unknown";
}

export function formatPromptMemorySource(source: string | undefined): string {
	if (source === "agent_workspace") return "Agent workspace";
	if (source === "root_workspace") return "Root workspace";
	return source || "unknown";
}

export function buildPromptMemorySummary(promptMemory: PromptMemoryData | null): string {
	if (!promptMemory) return "Unavailable";
	const parts: string[] = [formatPromptMemoryMode(promptMemory.mode)];
	if (promptMemory.snapshotActive) parts.push("snapshot active");
	parts.push(promptMemory.present ? `${Number(promptMemory.chars || 0).toLocaleString()} chars` : "empty");
	return parts.join(" \u00b7 ");
}

export function promptMemoryDetailParts(promptMemory: PromptMemoryData | null): string[] {
	if (!promptMemory) return [];
	const parts: string[] = [];
	if (promptMemory.fileSource) parts.push(`source ${formatPromptMemorySource(promptMemory.fileSource)}`);
	if (promptMemory.path) parts.push(promptMemory.path);
	return parts;
}

// ── Section renderers ────────────────────────────────────────

export function renderContextSessionSection(card: HTMLElement, data: ChatContextPayload): void {
	const sess = data.session ?? {};
	const sec = ctxSection("Session");
	sec.appendChild(ctxRow("Key", sess.key || "unknown", true));
	sec.appendChild(ctxRow("Messages", String(sess.messageCount || 0)));
	sec.appendChild(ctxRow("Model", sess.model || "default", true));
	if (sess.provider) sec.appendChild(ctxRow("Provider", sess.provider, true));
	if (sess.label) sec.appendChild(ctxRow("Label", sess.label));
	sec.appendChild(ctxRow("Tool Support", data.supportsTools === false ? "Disabled" : "Enabled"));
	card.appendChild(sec);
}

export function renderContextProjectSection(card: HTMLElement, data: ChatContextPayload): void {
	const proj = data.project;
	const sec = ctxSection("Project");
	if (proj) {
		sec.appendChild(ctxRow("Name", proj.label || "(unnamed)"));
		if (proj.directory) sec.appendChild(ctxRow("Directory", proj.directory, true));
		if (proj.systemPrompt) sec.appendChild(ctxRow("System Prompt", `${proj.systemPrompt.length} chars`));
		const ctxFiles = proj.contextFiles || [];
		if (ctxFiles.length > 0) {
			const fl = ctxEl("div", "ctx-section-title", `Context Files (${ctxFiles.length})`);
			fl.classList.add("spaced");
			sec.appendChild(fl);
			ctxFiles.forEach((f) => {
				const row = ctxEl("div", "ctx-file");
				row.appendChild(ctxEl("span", "ctx-file-path", f.path));
				row.appendChild(ctxEl("span", "ctx-file-size", formatBytes(f.size ?? 0)));
				sec.appendChild(row);
			});
		}
	} else {
		sec.appendChild(ctxEl("div", "ctx-empty", "No project bound to this session"));
	}
	card.appendChild(sec);
}

export function renderContextToolsSection(card: HTMLElement, data: ChatContextPayload): void {
	const tools = data.tools || [];
	const sec = ctxSection("Tools");
	if (data.supportsTools === false) {
		sec.appendChild(ctxEl("div", "ctx-disabled", "Tools disabled \u2014 model doesn't support tool calling"));
	} else if (tools.length > 0) {
		const wrap = ctxEl("div", "ctx-tool-wrap");
		tools.forEach((t) => {
			const tag = ctxEl("span", "ctx-tag");
			tag.appendChild(ctxEl("span", "ctx-tag-dot"));
			tag.appendChild(document.createTextNode(t.name));
			tag.title = t.description || "";
			wrap.appendChild(tag);
		});
		sec.appendChild(wrap);
		// In lazy registry mode the catalog lists every tool, but only a subset
		// of parameter schemas are loaded (get_tool + revealed). Surface that.
		const schemaCount = data.toolSchemaCount;
		if (typeof schemaCount === "number" && schemaCount < tools.length) {
			sec.appendChild(ctxEl("div", "ctx-empty", `${schemaCount} of ${tools.length} tool schemas loaded (lazy mode)`));
		}
	} else {
		sec.appendChild(ctxEl("div", "ctx-empty", "No tools registered"));
	}
	card.appendChild(sec);
}

export function renderContextSkillsSection(card: HTMLElement, data: ChatContextPayload): void {
	const skills = data.skills || [];
	const sec = ctxSection("Skills & Plugins");
	if (data.supportsTools === false) {
		sec.appendChild(ctxEl("div", "ctx-disabled", "Skills disabled \u2014 model doesn't support tool calling"));
	} else if (skills.length > 0) {
		const wrap = ctxEl("div", "ctx-tool-wrap");
		skills.forEach((s) => {
			const tag = ctxEl("span", "ctx-tag");
			const dot = ctxEl("span", "ctx-tag-dot");
			const isPlugin = s.source === "plugin";
			dot.style.background = isPlugin ? "var(--accent)" : "var(--success, #4a9)";
			tag.appendChild(dot);
			tag.appendChild(document.createTextNode(s.name));
			tag.title = (isPlugin ? "[Plugin] " : "[Skill] ") + (s.description || "");
			wrap.appendChild(tag);
		});
		sec.appendChild(wrap);
	} else {
		sec.appendChild(ctxEl("div", "ctx-empty", "No skills or plugins enabled"));
	}
	card.appendChild(sec);
}

export function renderContextMcpSection(card: HTMLElement, data: ChatContextPayload): void {
	const servers = data.mcpServers || [];
	const sec = ctxSection("MCP Tools");
	if (data.supportsTools === false) {
		sec.appendChild(ctxEl("div", "ctx-disabled", "MCP tools disabled \u2014 model doesn't support tool calling"));
	} else if (data.mcpDisabled) {
		sec.appendChild(ctxEl("div", "ctx-disabled", "MCP tools disabled for this session"));
	} else {
		const running = servers.filter((s) => s.state === "running");
		if (running.length > 0) {
			const wrap = ctxEl("div", "ctx-tool-wrap");
			running.forEach((s) => {
				const tag = ctxEl("span", "ctx-tag");
				const dot = ctxEl("span", "ctx-tag-dot");
				dot.style.background = "var(--ok)";
				tag.appendChild(dot);
				tag.appendChild(document.createTextNode(s.name));
				tag.title = `${s.tool_count} tool${s.tool_count !== 1 ? "s" : ""} \u2014 ${s.state}`;
				wrap.appendChild(tag);
			});
			sec.appendChild(wrap);
		} else {
			sec.appendChild(ctxEl("div", "ctx-empty", "No MCP tools running"));
		}
	}
	card.appendChild(sec);
}

export function renderContextSandboxSection(card: HTMLElement, data: ChatContextPayload): void {
	const sb = data.sandbox ?? {};
	const sec = ctxSection("Sandbox");
	sec.appendChild(ctxRow("Enabled", sb.enabled ? "yes" : "no", true));
	for (const [label, value, mono] of [
		["Backend", sb.backend, false],
		["Mode", sb.mode, false],
		["Scope", sb.scope, false],
		["Image", sb.image, true],
		["Container", sb.containerName, false],
	] as [string, string, boolean][]) {
		if (value) sec.appendChild(ctxRow(label, value, mono));
	}
	card.appendChild(sec);
}

interface TokenUsageView {
	sessionInput: number;
	sessionOutput: number;
	sessionCacheRead: number;
	sessionCacheWrite: number;
	sessionTotal: number;
	currentInput: number;
	currentOutput: number;
	currentCacheRead: number;
	currentCacheWrite: number;
	currentTotal: number;
	estimatedNextInput: number;
	contextWindow: number;
}

function tokenUsageView(usage: ChatContextTokenUsage): TokenUsageView {
	const sessionInput = usage.inputTokens || 0;
	const currentInput = usage.currentInputTokens || sessionInput;
	const currentOutput = usage.currentOutputTokens || 0;
	return {
		sessionInput,
		sessionOutput: usage.outputTokens || 0,
		sessionCacheRead: usage.cacheReadTokens || 0,
		sessionCacheWrite: usage.cacheWriteTokens || 0,
		sessionTotal: usage.total || 0,
		currentInput,
		currentOutput,
		currentCacheRead: usage.currentCacheReadTokens || 0,
		currentCacheWrite: usage.currentCacheWriteTokens || 0,
		currentTotal: usage.currentTotal || currentInput + currentOutput,
		estimatedNextInput: usage.estimatedNextInputTokens || currentInput,
		contextWindow: usage.contextWindow ?? 0,
	};
}

function appendPositiveTokenRow(section: HTMLElement, label: string, value: number): void {
	if (value > 0) section.appendChild(ctxRow(label, formatTokens(value), true));
}

function appendContextLeftRow(section: HTMLElement, usage: TokenUsageView): void {
	if (usage.contextWindow <= 0) return;
	const percentage = Math.max(0, 100 - Math.round((usage.estimatedNextInput / usage.contextWindow) * 100));
	section.appendChild(ctxRow("Context left", `${percentage}% of ${formatTokens(usage.contextWindow)}`, true));
}

export function renderContextTokensSection(card: HTMLElement, data: ChatContextPayload): void {
	const usage = tokenUsageView(data.tokenUsage ?? {});
	const section = ctxSection("Token Usage");
	section.appendChild(ctxRow("Session input", formatTokens(usage.sessionInput), true));
	section.appendChild(ctxRow("Session output", formatTokens(usage.sessionOutput), true));
	appendPositiveTokenRow(section, "Session cached input", usage.sessionCacheRead);
	appendPositiveTokenRow(section, "Session cache writes", usage.sessionCacheWrite);
	section.appendChild(ctxRow("Session total", formatTokens(usage.sessionTotal), true));
	section.appendChild(ctxRow("Current input", formatTokens(usage.currentInput), true));
	section.appendChild(ctxRow("Current output", formatTokens(usage.currentOutput), true));
	appendPositiveTokenRow(section, "Current cached input", usage.currentCacheRead);
	appendPositiveTokenRow(section, "Current cache writes", usage.currentCacheWrite);
	section.appendChild(ctxRow("Current total", formatTokens(usage.currentTotal), true));
	section.appendChild(ctxRow("Estimated next input", formatTokens(usage.estimatedNextInput), true));
	appendContextLeftRow(section, usage);
	card.appendChild(section);
}

export function renderContextPromptMemorySection(card: HTMLElement, data: ChatContextPayload): void {
	const pm = data.promptMemory || null;
	const sec = ctxSection("Prompt Memory");
	sec.appendChild(ctxRow("Status", buildPromptMemorySummary(pm)));
	if (pm) {
		sec.appendChild(ctxRow("Mode", formatPromptMemoryMode(pm.mode)));
		sec.appendChild(ctxRow("Present", pm.present ? "yes" : "no"));
		sec.appendChild(ctxRow("Chars", Number(pm.chars || 0).toLocaleString(), true));
		if (pm.fileSource) sec.appendChild(ctxRow("Source", formatPromptMemorySource(pm.fileSource)));
		if (pm.path) sec.appendChild(ctxRow("Path", pm.path, true));
	}
	card.appendChild(sec);
}

// ── Main context card renderer ───────────────────────────────

export function renderContextCard(data: ChatContextPayload): void {
	if (!S.chatMsgBox) return;
	slashInjectStyles();
	const card = ctxEl("div", "ctx-card");
	const header = ctxEl("div", "ctx-header");
	const icon = document.createElement("span");
	icon.className = "icon icon-settings-gear";
	header.appendChild(icon);
	header.appendChild(ctxEl("span", "ctx-header-title", "Context"));
	card.appendChild(header);
	if (data.supportsTools === false) {
		const warning = ctxEl("div", "ctx-warning");
		const warnIcon = document.createElement("span");
		warnIcon.className = "icon icon-warn-triangle-light";
		warning.appendChild(warnIcon);
		warning.appendChild(
			document.createTextNode(
				"Tools disabled \u2014 the current model doesn't support tool calling. Running in chat-only mode.",
			),
		);
		card.appendChild(warning);
	}
	renderContextSessionSection(card, data);
	renderContextProjectSection(card, data);
	renderContextSkillsSection(card, data);
	renderContextMcpSection(card, data);
	renderContextToolsSection(card, data);
	renderContextSandboxSection(card, data);
	renderContextPromptMemorySection(card, data);
	renderContextTokensSection(card, data);
	S.chatMsgBox.appendChild(card);
	smartScrollToBottom();
}
