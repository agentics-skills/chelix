// ── Chat RPC payload types ───────────────────────────────────

export interface ChatContextFile {
	path: string;
	size?: number;
}

export interface ChatContextSession {
	key?: string;
	messageCount?: number;
	model?: string;
	provider?: string;
	label?: string;
}

export interface ChatContextProject {
	label?: string;
	directory?: string;
	systemPrompt?: string;
	contextFiles?: ChatContextFile[];
}

export interface ChatContextSandbox {
	enabled?: boolean;
	backend?: string;
	mode?: string;
	scope?: string;
	image?: string;
	containerName?: string;
}

export interface ChatContextTokenUsage {
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	total?: number;
	currentInputTokens?: number;
	currentOutputTokens?: number;
	currentCacheReadTokens?: number;
	currentCacheWriteTokens?: number;
	currentTotal?: number;
	estimatedNextInputTokens?: number;
	contextWindow?: number;
}

export interface ChatWorkspaceFile {
	name?: string;
	truncated?: boolean;
	original_chars?: number;
	limit_chars?: number;
	truncated_chars?: number;
}

export interface PromptMemoryData {
	mode?: string;
	present?: boolean;
	chars?: number;
	fileSource?: string;
	path?: string;
	snapshotActive?: boolean;
}

export interface ChatContextPayload {
	session?: ChatContextSession;
	project?: ChatContextProject | null;
	tools?: Array<{ name: string; description?: string }>;
	toolSchemaCount?: number;
	skills?: Array<{ name: string; description?: string; source?: string }>;
	mcpServers?: Array<{ name: string; state?: string; tool_count?: number }>;
	mcpDisabled?: boolean;
	sandbox?: ChatContextSandbox;
	tokenUsage?: ChatContextTokenUsage;
	promptMemory?: PromptMemoryData | null;
	supportsTools?: boolean;
}

export interface ChatContextMessage {
	role?: string;
	content?: unknown;
	tool_calls?: Array<{
		id?: string;
		function?: { name?: string; arguments?: string };
	}>;
	tool_call_id?: string;
}

export interface ChatFullContextPayload {
	messages: ChatContextMessage[];
	llmOutputs: unknown[];
	messageCount: number;
	systemPromptChars: number;
	totalChars: number;
	truncated: boolean;
	workspaceFiles: ChatWorkspaceFile[];
	promptMemory: PromptMemoryData | null;
}

export interface ChatPromptMemoryRefreshPayload {
	ok: boolean;
	sessionKey: string;
	agentId: string;
	snapshotCleared: boolean;
	promptMemory: PromptMemoryData | null;
}
