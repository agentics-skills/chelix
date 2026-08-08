import { formatAssistantTokenUsage, formatTokenSpeed, tokenSpeedTone } from "./helpers";

export interface TerminalMetadataData {
	model?: string;
	provider?: string;
	reasoningEffort?: string;
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	durationMs?: number;
	replyMedium?: string;
	timestamp?: number;
	historyIndex?: number;
	runId?: string;
}

export interface TerminalMetadataSource extends TerminalMetadataData {
	created_at?: number;
	run_id?: string;
}

export interface TerminalMetadataOverrides {
	replyMedium?: string;
	timestamp?: number;
	historyIndex?: number;
	runId?: string;
}

export function terminalMetadataData(
	source: TerminalMetadataSource,
	overrides: TerminalMetadataOverrides = {},
): TerminalMetadataData {
	return {
		model: source.model,
		provider: source.provider,
		reasoningEffort: source.reasoningEffort,
		inputTokens: source.inputTokens,
		outputTokens: source.outputTokens,
		cacheReadTokens: source.cacheReadTokens,
		durationMs: source.durationMs,
		replyMedium: overrides.replyMedium ?? source.replyMedium,
		timestamp: overrides.timestamp ?? source.created_at ?? source.timestamp,
		historyIndex: overrides.historyIndex ?? source.historyIndex,
		runId: overrides.runId ?? source.run_id ?? source.runId,
	};
}

function metadataRowMatches(row: HTMLElement, data: TerminalMetadataData): boolean {
	const matchesHistory = Number.isInteger(data.historyIndex) && row.dataset.historyIndex === String(data.historyIndex);
	const matchesRun = Boolean(data.runId && row.dataset.runId === data.runId);
	return matchesHistory || matchesRun;
}

function removeExistingMetadata(container: HTMLElement, data: TerminalMetadataData): void {
	for (const child of container.children) {
		if (!(child instanceof HTMLElement && child.classList.contains("terminal-metadata"))) continue;
		if (metadataRowMatches(child, data)) child.remove();
	}
}

function metadataText(data: TerminalMetadataData): string {
	let text = data.provider ? `${data.provider} / ${data.model}` : data.model || "";
	if (data.reasoningEffort !== undefined) text += ` \u00b7 reasoning_effort: ${data.reasoningEffort || "off"}`;
	if (data.inputTokens || data.outputTokens) {
		text += ` \u00b7 ${formatAssistantTokenUsage(
			data.inputTokens || 0,
			data.outputTokens || 0,
			data.cacheReadTokens || 0,
		)}`;
	}
	return text;
}

function appendTokenSpeed(metadata: HTMLElement, data: TerminalMetadataData): void {
	const outputTokens = data.outputTokens || 0;
	const durationMs = data.durationMs || 0;
	const speedLabel = formatTokenSpeed(outputTokens, durationMs);
	if (!speedLabel) return;
	const speed = document.createElement("span");
	speed.className = "msg-token-speed";
	const tone = tokenSpeedTone(outputTokens, durationMs);
	if (tone) speed.classList.add(`msg-token-speed-${tone}`);
	speed.textContent = ` \u00b7 ${speedLabel}`;
	metadata.appendChild(speed);
}

function appendReplyMedium(metadata: HTMLElement, replyMedium: string | undefined): void {
	if (replyMedium !== "voice" && replyMedium !== "text") return;
	const badge = document.createElement("span");
	badge.className = "reply-medium-badge";
	badge.textContent = replyMedium;
	metadata.appendChild(badge);
}

function appendTimestamp(metadata: HTMLElement, timestamp: number | undefined): void {
	if (!timestamp) return;
	const timeEl = document.createElement("time");
	timeEl.className = "msg-footer-time";
	timeEl.setAttribute("data-epoch-ms", String(timestamp));
	timeEl.textContent = new Date(timestamp).toISOString();
	const wrap = document.createElement("span");
	wrap.className = "msg-footer-time";
	wrap.appendChild(document.createTextNode(" \u00b7 "));
	wrap.appendChild(timeEl);
	metadata.appendChild(wrap);
}

function buildMetadataRow(data: TerminalMetadataData): HTMLElement {
	const row = document.createElement("div");
	row.className = "terminal-metadata";
	if (Number.isInteger(data.historyIndex)) row.dataset.historyIndex = String(data.historyIndex);
	if (data.runId) row.dataset.runId = data.runId;

	const metadata = document.createElement("div");
	metadata.className = "msg-model-footer";
	const text = document.createElement("span");
	text.textContent = metadataText(data);
	metadata.appendChild(text);
	appendTokenSpeed(metadata, data);
	appendReplyMedium(metadata, data.replyMedium);
	appendTimestamp(metadata, data.timestamp);
	row.appendChild(metadata);
	return row;
}

function insertMetadataRow(container: HTMLElement, anchor: HTMLElement | null, row: HTMLElement): void {
	if (anchor?.parentElement === container) {
		anchor.insertAdjacentElement("afterend", row);
		return;
	}
	container.appendChild(row);
}

export function appendTerminalMetadata(
	container: HTMLElement | null,
	anchor: HTMLElement | null,
	data: TerminalMetadataData,
): HTMLElement | null {
	if (!(container && data.model)) return null;
	removeExistingMetadata(container, data);
	const row = buildMetadataRow(data);
	insertMetadataRow(container, anchor, row);
	return row;
}
