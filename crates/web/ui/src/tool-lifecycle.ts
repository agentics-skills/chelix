import type {
	AssistantHistoryMessage,
	ContextBudgetMetadata,
	ToolLifecycleEvent,
	ToolLifecyclePayload,
	ToolResult,
} from "./types/ws-events";

export interface ToolInvocationSnapshot {
	lifecycle: ToolLifecycleEvent;
	runId?: string;
	executionMode?: string;
	messageIndex?: number;
	assistantMessage?: AssistantHistoryMessage;
	assistantMessageIndex?: number;
	contextBudget?: ContextBudgetMetadata;
	accumulatedArguments?: string;
}

export interface ToolInvocationMetadata {
	runId?: string;
	executionMode?: string;
	messageIndex?: number;
	assistantMessage?: AssistantHistoryMessage;
	assistantMessageIndex?: number;
	contextBudget?: ContextBudgetMetadata;
	accumulatedArguments?: string;
}

export interface TerminalToolPresentation {
	success: boolean;
	rejected: boolean;
	result: ToolResult | string | null;
	error: string | null;
}

const TOOL_LIFECYCLE_STAGES = new Set([
	"created",
	"input_streaming",
	"input_ready",
	"waiting_for_execution",
	"executing",
	"execution_progress",
	"result_ready",
	"completed",
	"rejected",
	"cancelled",
]);

function isRecord(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isArguments(value: unknown): value is Record<string, unknown> {
	return isRecord(value);
}

function isLifecycleResult(value: unknown): value is string | null {
	return value === null || typeof value === "string";
}

function hasLifecycleBase(value: Record<string, unknown>): boolean {
	return (
		typeof value.toolCallId === "string" &&
		value.toolCallId.length > 0 &&
		typeof value.toolName === "string" &&
		Number.isSafeInteger(value.sequence) &&
		Number.isSafeInteger(value.emittedAtMs) &&
		(value.runId === undefined || typeof value.runId === "string") &&
		(value.contextBudget === undefined || isRecord(value.contextBudget)) &&
		typeof value.stage === "string" &&
		TOOL_LIFECYCLE_STAGES.has(value.stage)
	);
}

export function isToolLifecycleEvent(value: unknown): value is ToolLifecycleEvent {
	if (!(isRecord(value) && hasLifecycleBase(value))) return false;
	switch (value.stage) {
		case "created":
			return value.providerIndex === null || Number.isSafeInteger(value.providerIndex);
		case "input_streaming":
			return typeof value.argumentsDelta === "string";
		case "input_ready":
		case "waiting_for_execution":
			return isArguments(value.arguments);
		case "executing":
			return isArguments(value.arguments) && Number.isSafeInteger(value.startedAtMs);
		case "execution_progress":
			return isArguments(value.arguments) && Number.isSafeInteger(value.elapsedMs) && typeof value.message === "string";
		case "result_ready":
		case "completed":
			return (
				isArguments(value.arguments) &&
				typeof value.success === "boolean" &&
				isLifecycleResult(value.result) &&
				(value.error === null || typeof value.error === "string")
			);
		case "rejected":
			return isArguments(value.arguments) && typeof value.reason === "string" && typeof value.result === "string";
		case "cancelled":
			return (value.arguments === null || isArguments(value.arguments)) && typeof value.reason === "string";
		default:
			return false;
	}
}

export function isToolLifecyclePayload(value: unknown): value is ToolLifecyclePayload {
	return isRecord(value) && value.state === "tool_lifecycle" && isToolLifecycleEvent(value);
}

export function toToolLifecycleEvent(lifecycle: ToolLifecycleEvent): ToolLifecycleEvent {
	const base = {
		toolCallId: lifecycle.toolCallId,
		toolName: lifecycle.toolName,
		sequence: lifecycle.sequence,
		emittedAtMs: lifecycle.emittedAtMs,
		runId: lifecycle.runId,
		contextBudget: lifecycle.contextBudget,
	};
	switch (lifecycle.stage) {
		case "created":
			return { ...base, stage: lifecycle.stage, providerIndex: lifecycle.providerIndex };
		case "input_streaming":
			return {
				...base,
				stage: lifecycle.stage,
				argumentsDelta: lifecycle.argumentsDelta,
			};
		case "input_ready":
		case "waiting_for_execution":
			return { ...base, stage: lifecycle.stage, arguments: lifecycle.arguments };
		case "executing":
			return {
				...base,
				stage: lifecycle.stage,
				arguments: lifecycle.arguments,
				startedAtMs: lifecycle.startedAtMs,
			};
		case "execution_progress":
			return {
				...base,
				stage: lifecycle.stage,
				arguments: lifecycle.arguments,
				elapsedMs: lifecycle.elapsedMs,
				message: lifecycle.message,
			};
		case "result_ready":
		case "completed":
			return {
				...base,
				stage: lifecycle.stage,
				arguments: lifecycle.arguments,
				success: lifecycle.success,
				result: lifecycle.result,
				error: lifecycle.error,
			};
		case "rejected":
			return {
				...base,
				stage: lifecycle.stage,
				arguments: lifecycle.arguments,
				reason: lifecycle.reason,
				result: lifecycle.result,
			};
		case "cancelled":
			return {
				...base,
				stage: lifecycle.stage,
				arguments: lifecycle.arguments,
				reason: lifecycle.reason,
			};
	}
}

function definedMetadata(metadata: ToolInvocationMetadata): ToolInvocationMetadata {
	return Object.fromEntries(
		Object.entries(metadata).filter(([, value]) => value !== undefined),
	) as ToolInvocationMetadata;
}

export function reduceToolInvocation(
	previous: ToolInvocationSnapshot | undefined,
	lifecycle: ToolLifecycleEvent,
	metadata: ToolInvocationMetadata = {},
): ToolInvocationSnapshot {
	const normalized = toToolLifecycleEvent(lifecycle);
	const nextMetadata = definedMetadata(metadata);
	if (
		previous &&
		previous.lifecycle.toolCallId === normalized.toolCallId &&
		previous.lifecycle.sequence > normalized.sequence
	) {
		return { ...previous, ...nextMetadata };
	}
	const accumulatedArguments =
		nextMetadata.accumulatedArguments ??
		(normalized.stage === "input_streaming"
			? `${previous?.accumulatedArguments ?? ""}${normalized.argumentsDelta}`
			: previous?.accumulatedArguments);
	return {
		...previous,
		...nextMetadata,
		accumulatedArguments,
		lifecycle: normalized,
	};
}

export function toolInvocationKey(sessionKey: string, runId: string | undefined, toolCallId: string): string {
	return `${sessionKey}:${runId || ""}:${toolCallId}`;
}

export function toolLifecycleArguments(lifecycle: ToolLifecycleEvent, accumulatedArguments?: string): unknown {
	switch (lifecycle.stage) {
		case "created":
			return undefined;
		case "input_streaming": {
			const argumentsText = accumulatedArguments ?? lifecycle.argumentsDelta;
			try {
				return JSON.parse(argumentsText) as unknown;
			} catch {
				return argumentsText;
			}
		}
		case "input_ready":
		case "waiting_for_execution":
		case "executing":
		case "execution_progress":
		case "result_ready":
		case "completed":
		case "rejected":
			return lifecycle.arguments;
		case "cancelled":
			return lifecycle.arguments ?? undefined;
	}
}

export function isTerminalToolLifecycle(lifecycle: ToolLifecycleEvent): boolean {
	return lifecycle.stage === "completed" || lifecycle.stage === "rejected" || lifecycle.stage === "cancelled";
}

function decodeToolResult(result: string | null): ToolResult | string | null {
	if (result === null) return null;
	try {
		const parsed: unknown = JSON.parse(result);
		return isRecord(parsed) ? (parsed as ToolResult) : result;
	} catch {
		return result;
	}
}

export function terminalToolPresentation(lifecycle: ToolLifecycleEvent): TerminalToolPresentation | null {
	switch (lifecycle.stage) {
		case "completed":
			return {
				success: lifecycle.success,
				rejected: false,
				result: decodeToolResult(lifecycle.result),
				error: lifecycle.error,
			};
		case "rejected":
			return {
				success: false,
				rejected: true,
				result: decodeToolResult(lifecycle.result),
				error: lifecycle.reason,
			};
		case "cancelled":
			return {
				success: false,
				rejected: false,
				result: null,
				error: lifecycle.reason,
			};
		default:
			return null;
	}
}
