// ── User and default-agent setup step ───────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { EmojiPicker } from "../../emoji-picker";
import { refresh as refreshGon } from "../../gon";
import { parseAgentsListPayload, sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { targetValue } from "../../typed-events";
import type { ModelInfo } from "../../types/model";
import type { RpcResponse } from "../../types/rpc";
import { detectBrowserTimezone, ErrorPanel } from "../shared";

interface UnknownRecord {
	[key: string]: unknown;
}

interface AgentEntry extends UnknownRecord {
	id: string;
	name: string;
	emoji?: string | null;
	model: string;
	reasoning_effort: string;
	max_tools_threshold: number;
	soul?: string | null;
	subagent_prompt?: string | null;
}

interface UserLocation {
	latitude: number;
	longitude: number;
	place?: string | null;
	updated_at?: number | null;
}

interface UserProfile {
	name?: string | null;
	timezone?: string | null;
	location?: UserLocation | null;
}

interface IdentityLoadData {
	agent: AgentEntry | null;
	user: UserProfile;
	models: ModelInfo[];
	defaultMaxToolsThreshold: number;
}

type IdentityLoadResult = { ok: true; data: IdentityLoadData } | { ok: false; message: string };

const FIRST_AGENT_ID = "main";

function isRecord(value: unknown): value is UnknownRecord {
	return typeof value === "object" && value !== null;
}

function toAgentEntry(value: UnknownRecord): AgentEntry | null {
	const id = typeof value.id === "string" ? value.id : "";
	const name = typeof value.name === "string" ? value.name : "";
	const model = typeof value.model === "string" ? value.model : "";
	const reasoningEffort = typeof value.reasoning_effort === "string" ? value.reasoning_effort : "";
	const maxToolsThreshold = value.max_tools_threshold;
	if (!(id && name && model && reasoningEffort && typeof maxToolsThreshold === "number")) return null;
	return {
		...value,
		id,
		name,
		model,
		reasoning_effort: reasoningEffort,
		max_tools_threshold: maxToolsThreshold,
	};
}

function selectableModels(value: unknown): ModelInfo[] {
	if (!Array.isArray(value)) return [];
	return value
		.filter((entry): entry is ModelInfo => {
			if (!isRecord(entry)) return false;
			return (
				typeof entry.id === "string" &&
				entry.id.length > 0 &&
				Array.isArray(entry.reasoning_supported_efforts) &&
				entry.reasoning_supported_efforts.length > 0 &&
				entry.reasoning_supported_efforts.every((effort) => typeof effort === "string" && effort.length > 0)
			);
		})
		.filter((entry) => entry.disabled !== true);
}

function parseDefaultMaxToolsThreshold(value: unknown): number | null {
	if (!(isRecord(value) && isRecord(value.defaults))) return null;
	const threshold = value.defaults.max_tools_threshold;
	return typeof threshold === "number" && Number.isSafeInteger(threshold) && threshold >= 1 ? threshold : null;
}

function parseIdentityLoadResult(
	agentsResponse: RpcResponse<unknown>,
	userResponse: RpcResponse<unknown>,
	modelsResponse: RpcResponse<unknown>,
): IdentityLoadResult {
	if (!agentsResponse.ok) {
		return { ok: false, message: agentsResponse.error?.message || "Failed to load agents" };
	}
	if (!userResponse.ok) {
		return { ok: false, message: userResponse.error?.message || "Failed to load user profile" };
	}
	if (!modelsResponse.ok) {
		return { ok: false, message: modelsResponse.error?.message || "Failed to load configured models" };
	}

	const models = selectableModels(modelsResponse.payload);
	if (models.length === 0) {
		return {
				ok: false,
				message: "No configured models are available. Go back to LLM setup and configure a provider before continuing.",
			};
	}

	const defaultMaxToolsThreshold = parseDefaultMaxToolsThreshold(agentsResponse.payload);
	if (defaultMaxToolsThreshold === null) {
		return { ok: false, message: "Agent defaults returned an invalid max_tools_threshold." };
	}

	const parsed = parseAgentsListPayload(agentsResponse.payload as Parameters<typeof parseAgentsListPayload>[0]);
	let defaultAgent: AgentEntry | null = null;
	if (parsed.defaultId) {
		const defaultAgentValue = parsed.agents.find((entry) => entry.id === parsed.defaultId);
		defaultAgent = isRecord(defaultAgentValue) ? toAgentEntry(defaultAgentValue) : null;
		if (!defaultAgent) {
			return { ok: false, message: `Default agent "${parsed.defaultId}" is not defined under [agents]` };
		}
	} else if (parsed.agents.length > 0) {
		return { ok: false, message: "agents.default is required when agent entries exist" };
	}

	return {
		ok: true,
		data: {
			agent: defaultAgent,
			user: (userResponse.payload || {}) as UserProfile,
			models,
			defaultMaxToolsThreshold,
		},
	};
}

function agentConfigForSave(
	agent: AgentEntry | null,
	name: string,
	emoji: string,
	model: string,
	reasoningEffort: string,
	defaultMaxToolsThreshold: number,
): UnknownRecord {
	const source = agent || ({} as AgentEntry);
	const { id: _id, is_default: _isDefault, soul: _soul, subagent_prompt: _subagentPrompt, ...config } = source;
	return {
		...config,
		name: name.trim(),
		emoji: emoji.trim() || null,
		model,
		reasoning_effort: reasoningEffort,
		max_tools_threshold: agent?.max_tools_threshold ?? defaultMaxToolsThreshold,
	};
}

function confirmedAgentEntry(
	payload: unknown,
	id: string,
	config: UnknownRecord,
	soul: string,
	subagentPrompt: string,
): AgentEntry | null {
	if (isRecord(payload)) {
		const responseAgent = toAgentEntry(payload);
		if (responseAgent) return responseAgent;
	}
	return toAgentEntry({
		...config,
		id,
		soul,
		subagent_prompt: subagentPrompt,
	});
}

function locationForSave(location: UserLocation | null | undefined): UnknownRecord | null {
	if (!location) return null;
	return {
		latitude: location.latitude,
		longitude: location.longitude,
		place: location.place || null,
	};
}

export function IdentityStep({ onNext, onBack }: { onNext: () => void; onBack?: (() => void) | null }): VNode {
	const [agent, setAgent] = useState<AgentEntry | null>(null);
	const [user, setUser] = useState<UserProfile | null>(null);
	const [models, setModels] = useState<ModelInfo[]>([]);
	const [defaultMaxToolsThreshold, setDefaultMaxToolsThreshold] = useState<number | null>(null);
	const [userName, setUserName] = useState("");
	const [name, setName] = useState("");
	const [emoji, setEmoji] = useState("");
	const [model, setModel] = useState("");
	const [reasoningEffort, setReasoningEffort] = useState("");
	const [loading, setLoading] = useState(true);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const supportedReasoningEfforts: readonly string[] =
		models.find((configuredModel) => configuredModel.id === model)?.reasoning_supported_efforts ?? [];

	useEffect(() => {
		let cancelled = false;
		Promise.all([sendRpc("agents.list", {}), sendRpc("user.get", {}), sendRpc("models.list", {})]).then(
			([agentsResponse, userResponse, modelsResponse]) => {
				if (cancelled) return;
				const result = parseIdentityLoadResult(agentsResponse, userResponse, modelsResponse);
				if (!result.ok) {
					setError(result.message);
					setLoading(false);
					return;
				}

				setAgent(result.data.agent);
				setUser(result.data.user);
				setModels(result.data.models);
				setDefaultMaxToolsThreshold(result.data.defaultMaxToolsThreshold);
				setName(result.data.agent?.name || "");
				setEmoji(typeof result.data.agent?.emoji === "string" ? result.data.agent.emoji : "");
				setModel(result.data.agent?.model || "");
				setReasoningEffort(result.data.agent?.reasoning_effort || "");
				setUserName(typeof result.data.user.name === "string" ? result.data.user.name : "");
				setLoading(false);
			},
		);
		return () => {
			cancelled = true;
		};
	}, []);

	async function onSubmit(event: Event): Promise<void> {
		event.preventDefault();
		if (!name.trim()) {
			setError("Agent name is required.");
			return;
		}
		if (!userName.trim()) {
			setError("Your name is required.");
			return;
		}
		if (!models.some((configuredModel) => configuredModel.id === model)) {
			setError("Select an available configured model.");
			return;
		}
		if (!supportedReasoningEfforts.includes(reasoningEffort)) {
			setError("Select a reasoning effort supported by the model.");
			return;
		}
		if (!user) {
			setError("User profile is not loaded.");
			return;
		}
		if (defaultMaxToolsThreshold === null) {
			setError("Agent defaults are not loaded.");
			return;
		}

		setError(null);
		setSaving(true);
		const timezone = user.timezone || detectBrowserTimezone() || null;
		const agentId = agent?.id || FIRST_AGENT_ID;
		const agentConfig = agentConfigForSave(
			agent,
			name,
			emoji,
			model,
			reasoningEffort,
			defaultMaxToolsThreshold,
		);
		const soul = agent?.soul ?? "";
		const subagentPrompt = agent?.subagent_prompt ?? "";
		const agentResponse = await sendRpc(agent ? "agents.update" : "agents.create", {
			id: agentId,
			agent: agentConfig,
			soul,
			subagent_prompt: subagentPrompt,
		});
		if (!agentResponse.ok) {
			setSaving(false);
			setError(agentResponse.error?.message || `Failed to ${agent ? "update" : "create"} default agent`);
			return;
		}
		const savedAgent = confirmedAgentEntry(agentResponse.payload, agentId, agentConfig, soul, subagentPrompt);
		if (!savedAgent) {
			setSaving(false);
			setError("Agent save returned invalid state.");
			return;
		}
		setAgent(savedAgent);

		const userResponse = await sendRpc("user.update", {
			name: userName.trim(),
			timezone,
			location: locationForSave(user.location),
		});
		setSaving(false);
		if (!userResponse.ok) {
			setError(userResponse.error?.message || "Failed to save user profile");
			return;
		}
		setUser({ ...user, name: userName.trim(), timezone });

		await refreshGon();
		onNext();
	}

	if (loading) {
		return <div className="text-xs text-[var(--muted)]">Loading{"\u2026"}</div>;
	}

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:identity.title")}</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Tell us about yourself and configure your default agent.
			</p>
			<form onSubmit={onSubmit} className="flex flex-col gap-4">
				<div>
					<div className="text-xs text-[var(--muted)] mb-1">Your name *</div>
					<input
						type="text"
						className="provider-key-input w-full"
						value={userName}
						onInput={(event) => setUserName(targetValue(event))}
						placeholder="e.g. Alice"
						autofocus
					/>
				</div>
				<div className="grid grid-cols-1 gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:gap-x-4">
					<div className="min-w-0">
						<div className="text-xs text-[var(--muted)] mb-1">Agent name *</div>
						<input
							type="text"
							className="provider-key-input w-full"
							value={name}
							onInput={(event) => setName(targetValue(event))}
							placeholder="e.g. Rex"
						/>
					</div>
					<div>
						<div className="text-xs text-[var(--muted)] mb-1">Emoji</div>
						<EmojiPicker value={emoji} onChange={setEmoji} />
					</div>
				</div>
				<label className="flex flex-col gap-1">
					<span className="text-xs text-[var(--muted)]">Model *</span>
					<select
						className="provider-key-input w-full"
						value={model}
						disabled={models.length === 0}
						onChange={(event) => {
							setModel(targetValue(event));
							setReasoningEffort("");
						}}
					>
						<option value="">Select a model</option>
						{models.map((configuredModel) => (
							<option key={configuredModel.id} value={configuredModel.id}>
								{configuredModel.id}
							</option>
						))}
					</select>
				</label>
				<label className="flex flex-col gap-1">
					<span className="text-xs text-[var(--muted)]">Reasoning effort *</span>
					<select
						className="provider-key-input w-full"
						value={reasoningEffort}
						disabled={!model || supportedReasoningEfforts.length === 0}
						onChange={(event) => setReasoningEffort(targetValue(event))}
					>
						<option value="">Select reasoning effort</option>
						{supportedReasoningEfforts.map((effort) => (
							<option key={effort} value={effort}>
								{effort}
							</option>
						))}
					</select>
				</label>
				{error && <ErrorPanel message={error} />}
				<div className="flex flex-wrap items-center gap-3 mt-1">
					{onBack ? (
						<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
							{t("common:actions.back")}
						</button>
					) : null}
					<button
							key={`id-${saving}`}
							type="submit"
							className="provider-btn"
							disabled={saving || models.length === 0 || defaultMaxToolsThreshold === null}
						>
						{saving ? "Saving\u2026" : "Continue"}
					</button>
				</div>
			</form>
		</div>
	);
}
