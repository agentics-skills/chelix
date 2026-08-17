// ── WebSocket event types (discriminated union) ──────────────

/** All WebSocket event names emitted by the chelix gateway. */
export enum WsEventName {
	Chat = "chat",
	Error = "error",
	AuthCredentialsChanged = "auth.credentials_changed",
	CommandApprovalRequested = "command.approval.requested",
	LogsEntry = "logs.entry",
	SandboxPrepare = "sandbox.prepare",
	SandboxImageBuild = "sandbox.image.build",
	SandboxImageProvision = "sandbox.image.provision",
	BrowserImagePull = "browser.image.pull",
	ModelsUpdated = "models.updated",
	LocationRequest = "location.request",
	OperationProgress = "operation.progress",
	// Additional onEvent() events
	Tick = "tick",
	Session = "session",
	Channel = "channel",
	Presence = "presence",
	UpdateAvailable = "update.available",
	McpStatus = "mcp.status",
	HooksStatus = "hooks.status",
	MetricsUpdate = "metrics.update",
	SkillsInstallProgress = "skills.install.progress",
	PushSubscriptions = "push.subscriptions",
}

// ── Payload interfaces ───────────────────────────────────────

export type ReasoningContent = string | string[];

export function hasVisibleReasoning(content: ReasoningContent | null | undefined): boolean {
	return Array.isArray(content)
		? content.some((part) => part.trim().length > 0)
		: typeof content === "string" && content.trim().length > 0;
}

export function isReasoningContent(value: unknown): value is ReasoningContent {
	return typeof value === "string" || (Array.isArray(value) && value.every((part) => typeof part === "string"));
}

export interface ToolResult {
	stdout?: string;
	stderr?: string;
	exit_code?: number;
	output?: string;
	exitCode?: number;
	completed?: boolean;
	timedOut?: boolean;
	background?: boolean;
	terminalId?: string;
	message?: string;
	screenshot?: string;
	screenshot_scale?: number;
	document_ref?: string;
	filename?: string;
	mime_type?: string;
	size_bytes?: number;
	points?: MapPoint[];
	label?: string;
	map_links?: MapLinks;
}

export interface ToolError {
	detail?: string;
	message?: string;
	retryAfterMs?: number;
	type?: string;
}

export interface ContextBudgetMetadata {
	contextWindow: number;
	maxInputTokens: number;
	maxOutputTokens: number;
	compactionRatio: number;
	promptTokens: number;
	toolSchemaTokens: number;
	availableInputTokens: number;
	compactionBudget: number;
	usagePercent: number;
	compactionRequired: boolean;
}

export interface MapLinks {
	url?: string;
	google_maps?: string;
	apple_maps?: string;
	openstreetmap?: string;
	[key: string]: unknown;
}

export interface MapPoint {
	label?: string;
	latitude?: number;
	longitude?: number;
	map_links?: MapLinks;
}

export type ToolLifecycleStage =
	| "created"
	| "input_streaming"
	| "input_ready"
	| "waiting_for_execution"
	| "executing"
	| "execution_progress"
	| "result_ready"
	| "completed"
	| "rejected"
	| "cancelled";

interface ToolLifecycleBase {
	toolCallId: string;
	toolName: string;
	sequence: number;
	emittedAtMs: number;
	runId?: string;
	contextBudget?: ContextBudgetMetadata;
}

export type ToolLifecycleEvent = ToolLifecycleBase &
	(
		| { stage: "created"; providerIndex: number | null }
		| { stage: "input_streaming"; argumentsDelta: string }
		| { stage: "input_ready"; arguments: Record<string, unknown> }
		| { stage: "waiting_for_execution"; arguments: Record<string, unknown> }
		| { stage: "executing"; arguments: Record<string, unknown>; startedAtMs: number }
		| {
				stage: "execution_progress";
				arguments: Record<string, unknown>;
				elapsedMs: number;
				message: string;
		  }
		| {
				stage: "result_ready";
				arguments: Record<string, unknown>;
				success: boolean;
				result: string | null;
				error: string | null;
		  }
		| {
				stage: "completed";
				arguments: Record<string, unknown>;
				success: boolean;
				result: string | null;
				error: string | null;
		  }
		| {
				stage: "rejected";
				arguments: Record<string, unknown>;
				reason: string;
				result: string;
		  }
		| { stage: "cancelled"; arguments: Record<string, unknown> | null; reason: string }
	);

export type ToolLifecyclePayload = ChatPayload &
	ToolLifecycleEvent & {
		state: "tool_lifecycle";
		runId: string;
		sessionKey?: string;
		executionMode?: string;
		messageIndex?: number;
		assistantMessage?: AssistantHistoryMessage;
		assistantMessageIndex?: number;
	};

export type ActiveToolInvocation = ToolLifecycleEvent & {
	runId: string;
	executionMode?: string;
	accumulatedArguments?: string;
};

export interface AssistantHistoryMessage {
	role: "assistant";
	content?: string;
	model?: string;
	provider?: string;
	reasoningEffort?: string;
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	durationMs?: number;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
	tool_calls?: unknown[];
	reasoning?: ReasoningContent;
	audio?: string;
	run_id?: string;
	created_at?: number;
	seq?: number;
}

/** Persisted conversation-summarization checkpoint message. */
export interface CheckpointHistoryMessage {
	role: "checkpoint";
	summary?: string;
	model?: string;
	provider?: string;
	inputTokens?: number;
	outputTokens?: number;
	messagesSummarized?: number;
	created_at?: number;
	historyIndex?: number;
	[key: string]: unknown;
}

/** A user prompt queued while an agent run owns the session. */
export interface QueuedPrompt {
	id: string;
	sessionKey: string;
	position: number;
	preview: string;
	createdAt: number;
}

export interface ChatError {
	title?: string;
	detail?: string;
	message?: string;
	type?: string;
	retryAfterMs?: number;
	canContinue?: boolean;
}

export interface ChannelInfo {
	audio_filename?: string;
	[key: string]: unknown;
}

export interface PartialMessage {
	content?: string;
	reasoning?: ReasoningContent;
	reasoningEffort?: string;
	model?: string;
	provider?: string;
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	durationMs?: number;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
	tool_calls?: unknown[];
	audio?: string;
	run_id?: string;
	created_at?: number;
}

export interface ChatPayload {
	state?: string;
	sessionKey?: string;
	runId?: string;
	text?: ReasoningContent;
	model?: string;
	reasoningEffort?: string;
	provider?: string;
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	durationMs?: number;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
	reasoning?: ReasoningContent;
	audio?: string;
	audioWarning?: string | null;
	replyMedium?: string;
	messageIndex?: number;
	activeToolInvocations?: ActiveToolInvocation[];
	result?: ToolResult | string | null;
	error?: ChatError | string | null;
	message?: string;
	channel?: ChannelInfo;
	title?: string;
	phase?: string;
	mode?: string;
	seq?: number;
	/**
	 * Set on `user_message` events replayed from the prompt queue. Their seq
	 * was already used by the submitting client, whose optimistic bubble was
	 * dropped when the prompt was queued, so the message must render anyway.
	 */
	replayed?: boolean;
	retryAfterMs?: number;
	partialMessage?: PartialMessage;
	assistantMessage?: AssistantHistoryMessage;
	assistantMessageIndex?: number;
	checkpoint?: CheckpointHistoryMessage;
	contextBudget?: ContextBudgetMetadata;
	canContinue?: boolean;
	/** Full queue snapshot carried by `prompt_queue` events. */
	prompts?: QueuedPrompt[];
}

export interface ApprovalPayload {
	requestId: string;
	command: string;
}

export interface LogEntryPayload {
	level?: string;
	[key: string]: unknown;
}

export interface SandboxPhasePayload {
	phase?: string;
	error?: string;
	tag?: string;
	built?: boolean;
	count?: number;
	installed?: number;
	skipped?: number;
	image?: string;
	package_count?: number;
}

export interface OperationProgressPayload {
	operationId?: string;
	method?: string;
	kind?: string;
	sessionKey?: string | null;
	phase?: string;
	message?: string;
	current?: number | null;
	total?: number | null;
	done?: boolean;
}

export interface ModelsUpdatedPayload {
	phase?: string;
	[key: string]: unknown;
}

export interface WsErrorPayload {
	message?: string;
}

export interface LocationRequestPayload {
	requestId?: string;
	precision?: string;
}

export interface AuthCredentialsPayload {
	reason?: string;
}

export interface WsFrame {
	type: string;
	event?: string;
	payload?: Record<string, unknown>;
	stream?: unknown;
	done?: unknown;
	channel?: unknown;
}

export interface StreamMeta {
	stream: unknown;
	done: unknown;
	channel: unknown;
}

export interface AbortedPartialState {
	partial: PartialMessage | null;
	partialText: string;
	partialReasoning: ReasoningContent;
	hasVisiblePartial: boolean;
	hasTerminalToolBatch: boolean;
}

/** Maps event names to their payload types. */
export interface WsEventPayloadMap {
	[WsEventName.Chat]: ChatPayload;
	[WsEventName.Error]: WsErrorPayload;
	[WsEventName.AuthCredentialsChanged]: AuthCredentialsPayload;
	[WsEventName.CommandApprovalRequested]: ApprovalPayload;
	[WsEventName.LogsEntry]: LogEntryPayload;
	[WsEventName.SandboxPrepare]: SandboxPhasePayload;
	[WsEventName.SandboxImageBuild]: SandboxPhasePayload;
	[WsEventName.SandboxImageProvision]: SandboxPhasePayload;
	[WsEventName.BrowserImagePull]: SandboxPhasePayload;
	[WsEventName.ModelsUpdated]: ModelsUpdatedPayload;
	[WsEventName.LocationRequest]: LocationRequestPayload;
	[WsEventName.OperationProgress]: OperationProgressPayload;
	[WsEventName.Tick]: Record<string, unknown>;
	[WsEventName.Session]: Record<string, unknown>;
	[WsEventName.Channel]: Record<string, unknown>;
	[WsEventName.Presence]: Record<string, unknown>;
	[WsEventName.UpdateAvailable]: Record<string, unknown>;
	[WsEventName.McpStatus]: Record<string, unknown>;
	[WsEventName.HooksStatus]: Record<string, unknown>;
	[WsEventName.MetricsUpdate]: Record<string, unknown>;
	[WsEventName.SkillsInstallProgress]: Record<string, unknown>;
	[WsEventName.PushSubscriptions]: Record<string, unknown>;
}
