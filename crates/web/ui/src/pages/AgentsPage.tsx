// ── Settings > Agents page (Preact + JSX) ───────────────────

import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import { Loading } from "../components/forms/ListItem";
import { refresh as refreshGon } from "../gon";
import { parseAgentsListPayload, sendRpc } from "../helpers";
import { fetchSessions } from "../sessions";
import { targetValue } from "../typed-events";
import { confirmDialog } from "../ui";

interface UnknownRecord {
	[key: string]: unknown;
}

interface AgentEntry extends UnknownRecord {
	id: string;
	name: string;
	emoji?: string | null;
	description?: string | null;
	model?: string | null;
	max_tools_threshold: number;
	is_default?: boolean;
	soul?: string;
	subagent_prompt?: string;
}

interface AgentFormValues {
	id: string;
	name: string;
	emoji: string;
	description: string;
	model: string;
	maxToolsThreshold: string;
	soul: string;
	subagentPrompt: string;
}

interface AgentFormProps {
	agent: AgentEntry | null;
	defaultMaxToolsThreshold: number;
	onCancel: () => void;
	onSaved: () => void;
}

const WS_RETRY_LIMIT = 75;
const WS_RETRY_DELAY_MS = 200;
const FALLBACK_MAX_TOOLS_THRESHOLD = 128;

let containerRef: HTMLElement | null = null;

function isRecord(value: unknown): value is UnknownRecord {
	return typeof value === "object" && value !== null;
}

function optionalString(value: string): string | null {
	const trimmed = value.trim();
	return trimmed || null;
}

function parseDefaultMaxToolsThreshold(value: unknown): number {
	if (!(isRecord(value) && isRecord(value.defaults))) return FALLBACK_MAX_TOOLS_THRESHOLD;
	const threshold = value.defaults.max_tools_threshold;
	return typeof threshold === "number" && Number.isSafeInteger(threshold) && threshold >= 1
		? threshold
		: FALLBACK_MAX_TOOLS_THRESHOLD;
}

function toAgentEntry(value: UnknownRecord): AgentEntry | null {
	const id = typeof value.id === "string" ? value.id : "";
	const name = typeof value.name === "string" ? value.name : "";
	const maxToolsThreshold = value.max_tools_threshold;
	if (!(id && name && typeof maxToolsThreshold === "number")) return null;
	return {
		...value,
		id,
		name,
		max_tools_threshold: maxToolsThreshold,
	};
}

function agentConfigForSave(agent: AgentEntry | null, values: AgentFormValues): UnknownRecord {
	const source = agent || ({} as AgentEntry);
	const { id: _id, is_default: _isDefault, soul: _soul, subagent_prompt: _subagentPrompt, ...config } = source;
	return {
		...config,
		name: values.name.trim(),
		emoji: optionalString(values.emoji),
		description: optionalString(values.description),
		model: optionalString(values.model),
		max_tools_threshold: Number(values.maxToolsThreshold),
	};
}

export function initAgents(container: HTMLElement, subPath?: string | null): void {
	containerRef = container;
	render(<AgentsPageComponent subPath={subPath || undefined} />, container);
}

export function teardownAgents(): void {
	if (containerRef) render(null, containerRef);
	containerRef = null;
}

function AgentForm({ agent, defaultMaxToolsThreshold, onCancel, onSaved }: AgentFormProps): VNode {
	const [values, setValues] = useState<AgentFormValues>({
		id: agent?.id || "",
		name: agent?.name || "",
		emoji: agent?.emoji || "",
		description: agent?.description || "",
		model: agent?.model || "",
		maxToolsThreshold: String(agent?.max_tools_threshold || defaultMaxToolsThreshold),
		soul: agent?.soul || "",
		subagentPrompt: agent?.subagent_prompt || "",
	});
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	function setField<K extends keyof AgentFormValues>(key: K, value: AgentFormValues[K]): void {
		setValues((current) => ({ ...current, [key]: value }));
	}

	function save(): void {
		const id = values.id.trim();
		const name = values.name.trim();
		const threshold = Number(values.maxToolsThreshold);
		if (!name) {
			setError("Name is required.");
			return;
		}
		if (!id) {
			setError("ID is required.");
			return;
		}
		if (id === "default") {
			setError('ID "default" is reserved by the agent configuration table.');
			return;
		}
		if (!Number.isSafeInteger(threshold) || threshold < 1) {
			setError("Max tools threshold must be a positive integer.");
			return;
		}

		setSaving(true);
		setError(null);
		const method = agent ? "agents.update" : "agents.create";
		sendRpc(method, {
			id,
			agent: agentConfigForSave(agent, values),
			soul: values.soul,
			subagent_prompt: values.subagentPrompt,
		}).then((response) => {
			setSaving(false);
			if (response?.ok) {
				onSaved();
			} else {
				setError(response?.error?.message || `Failed to ${agent ? "update" : "create"} agent`);
			}
		});
	}

	return (
		<div className="flex-1 overflow-y-auto p-4">
			<div className="backend-card max-w-[680px] flex flex-col gap-4">
				<div className="flex items-center justify-between gap-3">
					<h2 className="text-lg font-medium text-[var(--text-strong)]">
						{agent ? `Edit ${agent.name}` : "Create Agent"}
					</h2>
					<button type="button" className="provider-btn provider-btn-sm" onClick={onCancel}>
						Cancel
					</button>
				</div>

				<div className="grid grid-cols-1 md:grid-cols-2 gap-3">
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">ID</span>
						<input
							className="provider-key-input"
							value={values.id}
							disabled={Boolean(agent)}
							onInput={(event) => setField("id", targetValue(event))}
							placeholder="e.g. writer, coder, researcher"
						/>
					</label>
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">Name</span>
						<input
							className="provider-key-input"
							value={values.name}
							onInput={(event) => setField("name", targetValue(event))}
							placeholder="Creative Writer"
						/>
					</label>
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">Emoji</span>
						<input
							className="provider-key-input"
							value={values.emoji}
							onInput={(event) => setField("emoji", targetValue(event))}
							placeholder="🤖"
						/>
					</label>
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">Model</span>
						<input
							className="provider-key-input"
							value={values.model}
							onInput={(event) => setField("model", targetValue(event))}
							placeholder="Optional model override"
						/>
					</label>
				</div>

				<label className="flex flex-col gap-1">
					<span className="text-xs text-[var(--muted)]">Description</span>
					<input
						className="provider-key-input"
						value={values.description}
						onInput={(event) => setField("description", targetValue(event))}
						placeholder="What this agent is for"
					/>
				</label>

				<label className="flex flex-col gap-1">
					<span className="text-xs text-[var(--muted)]">Max tools threshold</span>
					<input
						className="provider-key-input"
						type="number"
						min="1"
						step="1"
						value={values.maxToolsThreshold}
						onInput={(event) => setField("maxToolsThreshold", targetValue(event))}
					/>
				</label>

				<div className="grid grid-cols-1 lg:grid-cols-2 gap-3">
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">Soul</span>
						<textarea
							className="provider-key-input"
							value={values.soul}
							onInput={(event) => setField("soul", targetValue(event))}
							placeholder="System prompt used in chat"
							rows={10}
							style={{ resize: "vertical", fontFamily: "var(--font-mono)", fontSize: "0.75rem" }}
						/>
					</label>
					<label className="flex flex-col gap-1">
						<span className="text-xs text-[var(--muted)]">Sub-Agent system prompt</span>
						<textarea
							className="provider-key-input"
							value={values.subagentPrompt}
							onInput={(event) => setField("subagentPrompt", targetValue(event))}
							placeholder="System prompt used by spawn_agent"
							rows={10}
							style={{ resize: "vertical", fontFamily: "var(--font-mono)", fontSize: "0.75rem" }}
						/>
					</label>
				</div>

				{error && (
					<span className="text-xs" style={{ color: "var(--error)" }}>
						{error}
					</span>
				)}
				<div className="flex justify-end gap-2">
					<button type="button" className="provider-btn provider-btn-sm" onClick={onCancel} disabled={saving}>
						Cancel
					</button>
					<button
						type="button"
						className="provider-btn provider-btn-sm provider-btn-primary"
						onClick={save}
						disabled={saving}
					>
						{saving ? "Saving…" : agent ? "Save" : "Create"}
					</button>
				</div>
			</div>
		</div>
	);
}

function AgentCard({
	agent,
	defaultId,
	onEdit,
	onDelete,
	onSetDefault,
}: {
	agent: AgentEntry;
	defaultId: string;
	onEdit: () => void;
	onDelete: () => void;
	onSetDefault: () => void;
}): VNode {
	const isDefault = agent.id === defaultId;
	return (
		<div className="backend-card flex flex-col gap-3">
			<div className="flex items-start justify-between gap-3">
				<div className="flex items-start gap-3 min-w-0">
					<span className="text-xl" aria-hidden="true">
						{agent.emoji || "🤖"}
					</span>
					<div className="min-w-0">
						<div className="flex items-center gap-2 flex-wrap">
							<strong className="text-sm text-[var(--text-strong)]">{agent.name}</strong>
							<code className="text-xs text-[var(--muted)]">{agent.id}</code>
							{isDefault && <span className="recommended-badge">Default</span>}
						</div>
						{agent.description && <p className="text-xs text-[var(--muted)] mt-1">{agent.description}</p>}
						{agent.model && <p className="text-xs text-[var(--muted)] mt-1">Model: {agent.model}</p>}
					</div>
				</div>
			</div>
			<div className="flex items-center gap-2 flex-wrap">
				<button type="button" className="provider-btn provider-btn-sm" onClick={onEdit}>
					Edit
				</button>
				{!isDefault && (
					<>
						<button type="button" className="provider-btn provider-btn-sm" onClick={onSetDefault}>
							Set Default
						</button>
						<button type="button" className="provider-btn provider-btn-sm provider-btn-danger" onClick={onDelete}>
							Delete
						</button>
					</>
				)}
			</div>
		</div>
	);
}

function AgentsPageComponent({ subPath }: { subPath?: string }): VNode {
	const [agents, setAgents] = useState<AgentEntry[]>([]);
	const [defaultId, setDefaultId] = useState("");
	const [defaultMaxToolsThreshold, setDefaultMaxToolsThreshold] = useState(FALLBACK_MAX_TOOLS_THRESHOLD);
	const [editing, setEditing] = useState<"new" | AgentEntry | null>(subPath === "new" ? "new" : null);
	const [isLoading, setIsLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);

	function fetchAgents(): void {
		setIsLoading(true);
		let attempts = 0;
		function load(): void {
			sendRpc("agents.list", {}).then((response) => {
				if (
					(response?.error?.code === "UNAVAILABLE" || response?.error?.message === "WebSocket not connected") &&
					attempts < WS_RETRY_LIMIT
				) {
					attempts += 1;
					window.setTimeout(load, WS_RETRY_DELAY_MS);
					return;
				}
				setIsLoading(false);
				if (!response?.ok) {
					setError(response?.error?.message || "Failed to load agents");
					return;
				}
				const parsed = parseAgentsListPayload(response.payload as Parameters<typeof parseAgentsListPayload>[0]);
				setDefaultId(parsed.defaultId);
				setDefaultMaxToolsThreshold(parseDefaultMaxToolsThreshold(response.payload));
				setAgents(
					parsed.agents.map((entry) => toAgentEntry(entry)).filter((entry): entry is AgentEntry => entry !== null),
				);
				setError(null);
			});
		}
		load();
	}

	useEffect(() => {
		fetchAgents();
	}, []);

	function afterMutation(): void {
		setEditing(null);
		refreshGon();
		fetchSessions();
		fetchAgents();
	}

	function deleteAgent(agent: AgentEntry): void {
		confirmDialog(`Delete agent "${agent.name}"? Sessions using it will be reassigned to the default agent.`).then(
			(confirmed) => {
				if (!confirmed) return;
				sendRpc("agents.delete", { id: agent.id }).then((response) => {
					if (response?.ok) afterMutation();
					else setError(response?.error?.message || "Failed to delete agent");
				});
			},
		);
	}

	function setDefault(agent: AgentEntry): void {
		sendRpc("agents.set_default", { id: agent.id }).then((response) => {
			if (response?.ok) afterMutation();
			else setError(response?.error?.message || "Failed to set default agent");
		});
	}

	if (editing) {
		return (
			<AgentForm
				agent={editing === "new" ? null : editing}
				defaultMaxToolsThreshold={defaultMaxToolsThreshold}
				onCancel={() => setEditing(null)}
				onSaved={afterMutation}
			/>
		);
	}

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<div className="flex items-center gap-3 flex-wrap">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Agents</h2>
				<button type="button" className="provider-btn provider-btn-sm" onClick={() => setEditing("new")}>
					New Agent
				</button>
			</div>
			<p className="text-xs text-[var(--muted)] max-w-[680px]" style={{ margin: 0 }}>
				Every agent can be selected in chat and passed to <code>spawn_agent</code>. Soul is used in chat; Sub-Agent
				system prompt is used by spawned runs.
			</p>
			{error && (
				<span className="text-xs" style={{ color: "var(--error)" }}>
					{error}
				</span>
			)}
			{isLoading ? (
				<Loading message="Loading agents…" />
			) : (
				<section className="grid grid-cols-1 xl:grid-cols-2 gap-3 max-w-[1100px]" aria-label="Agents list">
					{agents.map((agent) => (
						<AgentCard
							key={agent.id}
							agent={agent}
							defaultId={defaultId}
							onEdit={() => setEditing(agent)}
							onDelete={() => deleteAgent(agent)}
							onSetDefault={() => setDefault(agent)}
						/>
					))}
				</section>
			)}
		</div>
	);
}
