// ── User and default-agent setup step ───────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { EmojiPicker } from "../../emoji-picker";
import { refresh as refreshGon } from "../../gon";
import { parseAgentsListPayload, sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { targetValue } from "../../typed-events";
import type { RpcResponse } from "../../types/rpc";
import { detectBrowserTimezone, ErrorPanel } from "../shared";

interface UnknownRecord {
	[key: string]: unknown;
}

interface AgentEntry extends UnknownRecord {
	id: string;
	name: string;
	emoji?: string | null;
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

function isRecord(value: unknown): value is UnknownRecord {
	return typeof value === "object" && value !== null;
}

function toAgentEntry(value: UnknownRecord): AgentEntry | null {
	const id = typeof value.id === "string" ? value.id : "";
	const name = typeof value.name === "string" ? value.name : "";
	if (!(id && name)) return null;
	return { ...value, id, name };
}

function agentConfigForSave(agent: AgentEntry, name: string, emoji: string): UnknownRecord {
	const { id: _id, is_default: _isDefault, soul: _soul, subagent_prompt: _subagentPrompt, ...config } = agent;
	return {
		...config,
		name: name.trim(),
		emoji: emoji.trim() || null,
	};
}

function locationForSave(location: UserLocation | null | undefined): UnknownRecord | null {
	if (!location) return null;
	return {
		latitude: location.latitude,
		longitude: location.longitude,
		place: location.place || null,
	};
}

function validateFields(name: string, userName: string): string | null {
	if (!name.trim()) return "Agent name is required.";
	if (!userName.trim()) return "Your name is required.";
	return null;
}

type IdentityLoadResult = { ok: true; agent: AgentEntry; user: UserProfile } | { ok: false; message: string };

function parseIdentityLoadResult(
	agentsResponse: RpcResponse<unknown>,
	userResponse: RpcResponse<unknown>,
): IdentityLoadResult {
	if (!agentsResponse.ok) {
		return { ok: false, message: agentsResponse.error?.message || "Failed to load agents" };
	}
	if (!userResponse.ok) {
		return { ok: false, message: userResponse.error?.message || "Failed to load user profile" };
	}

	const parsed = parseAgentsListPayload(agentsResponse.payload as Parameters<typeof parseAgentsListPayload>[0]);
	if (!parsed.defaultId) return { ok: false, message: "agents.default is empty" };

	const defaultAgentValue = parsed.agents.find((entry) => entry.id === parsed.defaultId);
	const defaultAgent = isRecord(defaultAgentValue) ? toAgentEntry(defaultAgentValue) : null;
	if (!defaultAgent) {
		return { ok: false, message: `Default agent "${parsed.defaultId}" is not defined under [agents]` };
	}

	return { ok: true, agent: defaultAgent, user: (userResponse.payload || {}) as UserProfile };
}

export function IdentityStep({ onNext, onBack }: { onNext: () => void; onBack?: (() => void) | null }): VNode {
	const [agent, setAgent] = useState<AgentEntry | null>(null);
	const [user, setUser] = useState<UserProfile | null>(null);
	const [userName, setUserName] = useState("");
	const [name, setName] = useState("");
	const [emoji, setEmoji] = useState("");
	const [loading, setLoading] = useState(true);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		Promise.all([sendRpc("agents.list", {}), sendRpc("user.get", {})]).then(([agentsResponse, userResponse]) => {
			if (cancelled) return;
			const result = parseIdentityLoadResult(agentsResponse, userResponse);
			if (!result.ok) {
				setError(result.message);
				setLoading(false);
				return;
			}

			setAgent(result.agent);
			setUser(result.user);
			setName(result.agent.name);
			setEmoji(typeof result.agent.emoji === "string" ? result.agent.emoji : "");
			setUserName(typeof result.user.name === "string" ? result.user.name : "");
			setLoading(false);
		});
		return () => {
			cancelled = true;
		};
	}, []);

	async function onSubmit(event: Event): Promise<void> {
		event.preventDefault();
		const validationError = validateFields(name, userName);
		if (validationError) {
			setError(validationError);
			return;
		}
		if (!(agent && user)) {
			setError("Default agent or user profile is not loaded.");
			return;
		}

		setError(null);
		setSaving(true);
		const timezone = user.timezone || detectBrowserTimezone() || null;
		const agentResponse = await sendRpc("agents.update", {
			id: agent.id,
			agent: agentConfigForSave(agent, name, emoji),
			soul: agent.soul ?? "",
			subagent_prompt: agent.subagent_prompt ?? "",
		});
		if (!agentResponse.ok) {
			setSaving(false);
			setError(agentResponse.error?.message || "Failed to save default agent");
			return;
		}

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
				Tell us about yourself and customise your default agent.
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
				{error && <ErrorPanel message={error} />}
				<div className="flex flex-wrap items-center gap-3 mt-1">
					{onBack ? (
						<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
							{t("common:actions.back")}
						</button>
					) : null}
					<button key={`id-${saving}`} type="submit" className="provider-btn" disabled={saving}>
						{saving ? "Saving\u2026" : "Continue"}
					</button>
				</div>
			</form>
		</div>
	);
}
