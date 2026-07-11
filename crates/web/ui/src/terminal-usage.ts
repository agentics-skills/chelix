export interface TerminalUsage {
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
}

export function terminalContextTokens(usage: TerminalUsage): number {
	return (
		(usage.requestInputTokens ?? usage.inputTokens ?? 0) +
		(usage.requestOutputTokens ?? usage.outputTokens ?? 0) +
		(usage.requestCacheReadTokens ?? usage.cacheReadTokens ?? 0) +
		(usage.requestCacheWriteTokens ?? usage.cacheWriteTokens ?? 0)
	);
}
