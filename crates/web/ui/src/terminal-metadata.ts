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

export function appendTerminalMetadata(
	container: HTMLElement | null,
	anchor: HTMLElement | null,
	data: TerminalMetadataData,
): HTMLElement | null {
	if (!(container && data.model)) return null;
	for (const child of container.children) {
		if (!(child instanceof HTMLElement && child.classList.contains("terminal-metadata"))) continue;
		if (
			(Number.isInteger(data.historyIndex) && child.dataset.historyIndex === String(data.historyIndex)) ||
			(data.runId && child.dataset.runId === data.runId)
		) {
			child.remove();
		}
	}
	const row = document.createElement("div");
	row.className = "terminal-metadata";
	if (Number.isInteger(data.historyIndex)) row.dataset.historyIndex = String(data.historyIndex);
	if (data.runId) row.dataset.runId = data.runId;

	const metadata = document.createElement("div");
	metadata.className = "msg-model-footer";
	let metadataText = data.provider ? `${data.provider} / ${data.model}` : data.model;
	if (data.reasoningEffort !== undefined) {
		metadataText += ` \u00b7 reasoning_effort: ${data.reasoningEffort || "off"}`;
	}
	if (data.inputTokens || data.outputTokens) {
		metadataText += ` \u00b7 ${formatAssistantTokenUsage(
			data.inputTokens || 0,
			data.outputTokens || 0,
			data.cacheReadTokens || 0,
		)}`;
	}
	const text = document.createElement("span");
	text.textContent = metadataText;
	metadata.appendChild(text);

	const speedLabel = formatTokenSpeed(data.outputTokens || 0, data.durationMs || 0);
	if (speedLabel) {
		const speed = document.createElement("span");
		speed.className = "msg-token-speed";
		const tone = tokenSpeedTone(data.outputTokens || 0, data.durationMs || 0);
		if (tone) speed.classList.add(`msg-token-speed-${tone}`);
		speed.textContent = ` \u00b7 ${speedLabel}`;
		metadata.appendChild(speed);
	}

	if (data.replyMedium === "voice" || data.replyMedium === "text") {
		const badge = document.createElement("span");
		badge.className = "reply-medium-badge";
		badge.textContent = data.replyMedium;
		metadata.appendChild(badge);
	}

	if (data.timestamp) {
		const timeEl = document.createElement("time");
		timeEl.className = "msg-footer-time";
		timeEl.setAttribute("data-epoch-ms", String(data.timestamp));
		timeEl.textContent = new Date(data.timestamp).toISOString();
		const wrap = document.createElement("span");
		wrap.className = "msg-footer-time";
		wrap.appendChild(document.createTextNode(" \u00b7 "));
		wrap.appendChild(timeEl);
		metadata.appendChild(wrap);
	}

	row.appendChild(metadata);
	if (anchor?.parentElement === container) {
		anchor.insertAdjacentElement("afterend", row);
	} else {
		container.appendChild(row);
	}
	return row;
}
