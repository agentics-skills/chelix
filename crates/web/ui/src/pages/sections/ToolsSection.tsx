// ── Tools section ─────────────────────────────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";

interface ToolEntry {
	name: string;
	description?: string | null;
}

interface ToolGroup {
	label: string;
	tools: ToolEntry[];
}

interface ResolvedToolsSession {
	model: string;
	provider: string;
	label?: string | null;
}

interface ResolvedToolsSandbox {
	enabled: boolean;
	backend: string;
}

interface ResolvedToolsContextPayload {
	session: ResolvedToolsSession;
	tools: ToolEntry[];
	sandbox: ResolvedToolsSandbox;
	supportsTools: boolean;
}

const TOOLS_RPC_TIMEOUT_MS = 30_000;

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === "object" && value !== null && !Array.isArray(value);
}

function isToolEntry(value: unknown): value is ToolEntry {
	if (!isRecord(value) || typeof value.name !== "string" || !value.name) return false;
	return value.description === undefined || value.description === null || typeof value.description === "string";
}

function parseToolsContextPayload(payload: unknown): ResolvedToolsContextPayload {
	if (!isRecord(payload)) throw new Error("Invalid tools overview response.");
	const session = payload.session;
	const sandbox = payload.sandbox;
	const tools = payload.tools;
	if (
		!isRecord(session) ||
		typeof session.model !== "string" ||
		!session.model ||
		typeof session.provider !== "string" ||
		!session.provider ||
		(session.label !== undefined && session.label !== null && typeof session.label !== "string") ||
		!isRecord(sandbox) ||
		typeof sandbox.enabled !== "boolean" ||
		typeof sandbox.backend !== "string" ||
		!sandbox.backend ||
		!Array.isArray(tools) ||
		!tools.every(isToolEntry) ||
		typeof payload.supportsTools !== "boolean"
	) {
		throw new Error("Invalid tools overview response.");
	}
	return {
		session: {
			model: session.model,
			provider: session.provider,
			label: session.label,
		},
		tools,
		sandbox: {
			enabled: sandbox.enabled,
			backend: sandbox.backend,
		},
		supportsTools: payload.supportsTools,
	};
}

function parseToolsRpcResponse(response: unknown): ResolvedToolsContextPayload {
	if (!isRecord(response) || typeof response.ok !== "boolean") {
		throw new Error("Invalid tools overview response.");
	}
	if (!response.ok) {
		const error = response.error;
		if (!isRecord(error) || typeof error.message !== "string" || !error.message) {
			throw new Error("Invalid tools overview response.");
		}
		throw new Error(error.message);
	}
	return parseToolsContextPayload(response.payload);
}

async function requestToolsContext(sessionKey: string): Promise<ResolvedToolsContextPayload> {
	const controller = new AbortController();
	const timeout = window.setTimeout(() => controller.abort(), TOOLS_RPC_TIMEOUT_MS);
	try {
		const response = await fetch("/api/rpc", {
			method: "POST",
			headers: {
				Accept: "application/json",
				"Content-Type": "application/json",
			},
			body: JSON.stringify({
				method: "chat.context",
				params: {},
				sessionKey,
			}),
			signal: controller.signal,
		});
		if (!response.ok) {
			throw new Error(`Failed to load tools overview (HTTP ${response.status}).`);
		}
		const rpcResponse: unknown = await response.json();
		return parseToolsRpcResponse(rpcResponse);
	} catch (error: unknown) {
		if (controller.signal.aborted) {
			throw new Error("Tools overview request timed out. Try again.");
		}
		if (error instanceof Error) throw error;
		throw new Error("Failed to load tools overview.");
	} finally {
		window.clearTimeout(timeout);
	}
}

export function toolsOverviewCategory(name: string | undefined): string {
	if (typeof name !== "string" || !name) return "Core";
	if (name.startsWith("mcp__")) return "MCP";
	if (name === "execute_command" || name.startsWith("sandbox")) {
		return "Execution";
	}
	if (name.startsWith("session") || name.startsWith("sessions_")) return "Sessions";
	if (name.startsWith("memory") || name.includes("memory")) return "Memory";
	if (name.startsWith("browser") || name.startsWith("web_") || name.includes("screenshot") || name.includes("fetch")) {
		return "Web & Browser";
	}
	if (name.startsWith("skill") || name.includes("skill")) return "Skills";
	return "Core";
}

export function groupToolsForOverview(tools: ToolEntry[]): ToolGroup[] {
	const grouped = new Map<string, ToolEntry[]>();
	for (const tool of tools) {
		const category = toolsOverviewCategory(tool.name);
		if (!grouped.has(category)) grouped.set(category, []);
		grouped.get(category)?.push(tool);
	}
	const order = ["Execution", "Sessions", "Memory", "Web & Browser", "Skills", "MCP", "Core"];
	const groups: ToolGroup[] = [];
	for (const label of order) {
		const entries = grouped.get(label);
		if (!entries) continue;
		groups.push({
			label,
			tools: entries.slice().sort((left, right) => left.name.localeCompare(right.name)),
		});
	}
	return groups;
}

interface ToolCallingSummaryProps {
	supportsTools: boolean;
	toolCount: number;
}

function ToolCallingSummary({ supportsTools, toolCount }: ToolCallingSummaryProps): VNode {
	return (
		<div className="rounded border border-[var(--border)] bg-[var(--surface)] p-4">
			<div className="text-xs uppercase tracking-wide text-[var(--muted)]">Tool Calling</div>
			<div className="mt-2 flex items-center gap-2 flex-wrap">
				<span className={`provider-item-badge ${supportsTools ? "configured" : "warning"}`}>
					{supportsTools ? "Enabled" : "Disabled"}
				</span>
				<span className="text-sm font-medium text-[var(--text)]">
					{toolCount} registered tool{toolCount === 1 ? "" : "s"}
				</span>
			</div>
			<div className="text-xs text-[var(--muted)] mt-2 leading-relaxed">
				{supportsTools
					? "Built-in, MCP, and runtime-routed tools available to the active model."
					: "The configured tool mode is off, so the agent cannot call tools in this session."}
			</div>
		</div>
	);
}

function ActiveModelSummary({ session }: { session: ResolvedToolsSession }): VNode {
	const sessionText = session.label ? ` Session: ${session.label}.` : "";
	return (
		<div className="rounded border border-[var(--border)] bg-[var(--surface)] p-4">
			<div className="text-xs uppercase tracking-wide text-[var(--muted)]">Active Model</div>
			<div className="mt-2 text-sm font-medium text-[var(--text)] break-words">{session.model}</div>
			<div className="text-xs text-[var(--muted)] mt-2 leading-relaxed">
				Provider: {session.provider}.{sessionText}
			</div>
		</div>
	);
}

function ExecutionRuntimeSummary({ sandbox }: { sandbox: ResolvedToolsSandbox }): VNode {
	return (
		<div className="rounded border border-[var(--border)] bg-[var(--surface)] p-4">
			<div className="text-xs uppercase tracking-wide text-[var(--muted)]">Execution Runtime</div>
			<div className="mt-2 text-sm font-medium text-[var(--text)]">{sandbox.enabled ? "Sandbox" : "Host"}</div>
			<div className="text-xs text-[var(--muted)] mt-2 leading-relaxed">
				{sandbox.enabled ? `Sandbox backend: ${sandbox.backend}. ` : ""}
				The <code className="text-[var(--text)]">execute_command</code> tool runs through the managed tools service.
			</div>
		</div>
	);
}

function ToolCallingWarning({ supportsTools }: { supportsTools: boolean }): VNode | null {
	if (supportsTools) return null;
	return (
		<div className="rounded border border-[var(--warn)] bg-[var(--surface2)] p-3 max-w-[1100px]">
			<div className="text-xs text-[var(--muted)] leading-relaxed">
				Tools are unavailable because the configured tool mode is off.
			</div>
		</div>
	);
}

function ToolOverviewCard({ tool }: { tool: ToolEntry }): VNode {
	return (
		<div className="rounded border border-[var(--border)] bg-[var(--surface2)] p-3">
			<div className="flex items-center justify-between gap-2 flex-wrap">
				<div className="text-xs font-medium text-[var(--text)] break-words">{tool.name}</div>
				{tool.name.startsWith("mcp__") ? <span className="provider-item-badge configured">MCP</span> : null}
			</div>
			<div className="text-xs text-[var(--muted)] mt-1 leading-relaxed">
				{tool.description || "No description provided."}
			</div>
		</div>
	);
}

function ToolGroupOverview({ group }: { group: ToolGroup }): VNode {
	return (
		<div>
			<div className="text-xs uppercase tracking-wide text-[var(--muted)] mb-2">
				{group.label} {"\u00b7"} {group.tools.length}
			</div>
			<div className="flex flex-col gap-2">
				{group.tools.map((tool) => (
					<ToolOverviewCard key={tool.name} tool={tool} />
				))}
			</div>
		</div>
	);
}

function RegisteredTools({ groups, toolCount }: { groups: ToolGroup[]; toolCount: number }): VNode {
	return (
		<div className="rounded border border-[var(--border)] bg-[var(--surface)] p-4 max-w-[1100px]">
			<div className="flex items-center justify-between gap-2 flex-wrap">
				<h3 className="text-sm font-medium text-[var(--text-strong)] m-0">Registered Tools</h3>
				<span className="provider-item-badge muted">{toolCount}</span>
			</div>
			{groups.length > 0 ? (
				<div className="mt-3 flex flex-col gap-3">
					{groups.map((group) => (
						<ToolGroupOverview key={group.label} group={group} />
					))}
				</div>
			) : (
				<div className="text-xs text-[var(--muted)] mt-3">No tools are currently exposed to this session.</div>
			)}
		</div>
	);
}

export function ToolsSection(): VNode {
	const [loadingTools, setLoadingTools] = useState(true);
	const [toolData, setToolData] = useState<ResolvedToolsContextPayload | null>(null);
	const [toolsErr, setToolsErr] = useState<string | null>(null);

	function loadToolsOverview(): void {
		setLoadingTools(true);
		setToolData(null);
		setToolsErr(null);
		const sessionKey = localStorage.getItem("chelix-session");
		if (!sessionKey) {
			setLoadingTools(false);
			setToolsErr("Open a chat session before viewing its tools.");
			return;
		}
		requestToolsContext(sessionKey)
			.then((payload) => {
				setToolData(payload);
				setLoadingTools(false);
			})
			.catch((error: Error) => {
				setLoadingTools(false);
				setToolsErr(error.message);
			});
	}

	useEffect(() => {
		loadToolsOverview();
	}, []);

	const toolGroups = toolData ? groupToolsForOverview(toolData.tools) : [];

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<div className="flex items-start justify-between gap-3 flex-wrap max-w-[1100px]">
				<div className="min-w-0">
					<h2 className="text-lg font-medium text-[var(--text-strong)]">Tools</h2>
					<p className="text-xs text-[var(--muted)] mt-1 max-w-[900px] leading-relaxed">
						This page shows the effective tool inventory for the active session and model. Change the current LLM, or
						disable MCP for a session, and the inventory here will change with it.
					</p>
				</div>
				<button
					type="button"
					className="provider-btn provider-btn-secondary"
					onClick={loadToolsOverview}
					disabled={loadingTools}
				>
					{loadingTools ? "Refreshing\u2026" : "Refresh"}
				</button>
			</div>

			{toolsErr ? <div className="text-xs text-[var(--error)] max-w-[1100px]">{toolsErr}</div> : null}

			{toolData ? (
				<>
					<div className="grid gap-4 md:grid-cols-2 max-w-[1100px]">
						<ToolCallingSummary supportsTools={toolData.supportsTools} toolCount={toolData.tools.length} />
						<ActiveModelSummary session={toolData.session} />
						<ExecutionRuntimeSummary sandbox={toolData.sandbox} />
					</div>

					<ToolCallingWarning supportsTools={toolData.supportsTools} />
					<RegisteredTools groups={toolGroups} toolCount={toolData.tools.length} />
				</>
			) : null}
		</div>
	);
}
