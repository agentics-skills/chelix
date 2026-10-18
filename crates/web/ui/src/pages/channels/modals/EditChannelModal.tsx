// ── Edit channel modal ───────────────────────────────────────

import { type Signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { useEffect } from "preact/hooks";

import { parseChannelConfigPatch } from "../../../channel-utils";
import { sendRpc } from "../../../helpers";
import { targetValue } from "../../../typed-events";
import { ChannelType } from "../../../types/channel";
import { Modal } from "../../../ui";
import { type ChannelConfig, channelType, editingChannel, loadChannels } from "../../ChannelsPage";
import {
	AdvancedConfigPatchField,
	AllowlistInput,
	ChannelModelFields,
	resolveChannelModelSelection,
} from "../ChannelFields";

interface EditChannelDraft {
	model: string;
	reasoningEffort: string;
	agent: string;
	allowlist: string[];
}

function configString(value: unknown, fallback = ""): string {
	return typeof value === "string" && value ? value : fallback;
}

function firstConfigArray(values: unknown[]): string[] {
	return (values.find(Array.isArray) as string[] | undefined) || [];
}

function channelEditDraft(config: ChannelConfig): EditChannelDraft {
	return {
		model: configString(config.model_override?.model),
		reasoningEffort: configString(config.model_override?.reasoning_effort),
		agent: configString(config.agent_id),
		allowlist: firstConfigArray([config.allowlist, config.allowed_pubkeys]),
	};
}

function formFieldValue(form: HTMLElement, field: string, fallback: string): string {
	return (
		(form.querySelector(`[data-field=${field}]`) as HTMLInputElement | HTMLSelectElement | null)?.value || fallback
	);
}

function applyChannelCredentials(config: ChannelConfig, channelKind: string, current: ChannelConfig): void {
	if (channelKind === ChannelType.Telegram) config.token = current.token || "";
}

function buildChannelUpdateConfig(
	form: HTMLElement,
	channelKind: string,
	current: ChannelConfig,
	draft: EditChannelDraft,
): ChannelConfig {
	const config: ChannelConfig = {
		dm_policy: formFieldValue(form, "dmPolicy", "allowlist"),
		allowlist: draft.allowlist,
		agent_id: draft.agent || null,
		mention_mode: formFieldValue(form, "mentionMode", "mention"),
	};
	applyChannelCredentials(config, channelKind, current);
	return config;
}

interface ChannelPolicyFieldsProps {
	config: ChannelConfig;
}

function ChannelPolicyFields({ config }: ChannelPolicyFieldsProps): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">DM Policy</span>
				<select data-field="dmPolicy" className="channel-select" value={configString(config.dm_policy, "allowlist")}>
					<option value="allowlist">Allowlist only</option>
					<option value="open">Open (anyone)</option>
					<option value="disabled">Disabled</option>
				</select>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">Group Mention Mode</span>
				<select
					data-field="mentionMode"
					className="channel-select"
					value={configString(config.mention_mode, "mention")}
				>
					<option value="mention">Must @mention bot</option>
					<option value="always">Always respond</option>
					<option value="none">Don't respond in groups</option>
				</select>
			</label>
		</>
	);
}

interface ChannelAllowlistFieldsProps {
	allowlist: Signal<string[]>;
}

function ChannelAllowlistFields({ allowlist }: ChannelAllowlistFieldsProps): VNode {
	return (
		<>
			<span className="text-xs text-[var(--muted)]">DM Allowlist</span>
			<AllowlistInput
				ariaLabel="DM Allowlist"
				value={allowlist.value}
				onChange={(value) => {
					allowlist.value = value;
				}}
			/>
		</>
	);
}

export function EditChannelModal(): VNode | null {
	const ch = editingChannel.value;
	const error = useSignal("");
	const saving = useSignal(false);
	const editModel = useSignal("");
	const editReasoningEffort = useSignal("");
	const editAgent = useSignal("");
	const agentsList = useSignal<Array<{ id: string; name: string; emoji?: string }>>([]);
	const allowlistItems = useSignal<string[]>([]);
	const editAdvancedConfigPatch = useSignal("");

	useEffect(() => {
		const draft = channelEditDraft(ch?.config || {});
		editModel.value = draft.model;
		editReasoningEffort.value = draft.reasoningEffort;
		editAgent.value = draft.agent;
		allowlistItems.value = draft.allowlist;
		editAdvancedConfigPatch.value = "";
	}, [ch]);

	useEffect(() => {
		sendRpc("agents.list", {}).then((res) => {
			if (res?.ok) {
				const payload = res.payload as { agents?: Array<{ id: string; name: string; emoji?: string }> };
				agentsList.value = payload?.agents || [];
			}
		});
	}, []);

	if (!ch) return null;

	const cfg = ch.config || {};
	const chType = channelType(ch.type);
	const isTelegram = chType === ChannelType.Telegram;

	function currentDraft(): EditChannelDraft {
		return {
			model: editModel.value,
			reasoningEffort: editReasoningEffort.value,
			agent: editAgent.value,
			allowlist: allowlistItems.value,
		};
	}

	function onSave(e: Event): void {
		e.preventDefault();
		const form = (e.target as HTMLElement).closest(".channel-form") as HTMLElement;
		const advancedPatch = parseChannelConfigPatch(editAdvancedConfigPatch.value);
		if (!advancedPatch.ok) {
			error.value = advancedPatch.error;
			return;
		}
		const draft = currentDraft();
		const modelSelection = resolveChannelModelSelection(draft.model, draft.reasoningEffort);
		if (!modelSelection.ok) {
			error.value = modelSelection.error;
			return;
		}
		error.value = "";
		if (!ch) return;
		saving.value = true;
		const updateConfig = buildChannelUpdateConfig(form, chType, cfg, draft);
		Object.assign(updateConfig, modelSelection.config, advancedPatch.value);
		sendRpc("channels.update", {
			type: channelType(ch.type),
			account_id: ch.account_id,
			config: updateConfig,
		}).then((res) => {
			saving.value = false;
			if (res?.ok) {
				editingChannel.value = null;
				loadChannels();
			} else {
				error.value =
					(res?.error as { message?: string; detail?: string })?.message ||
					(res?.error as { detail?: string })?.detail ||
					"Failed to update channel.";
			}
		});
	}

	return (
		<Modal
			show={true}
			onClose={() => {
				editingChannel.value = null;
			}}
			title="Edit Telegram Channel"
		>
			<div className="channel-form">
				<div className="text-sm text-[var(--text-strong)]">{ch.name || ch.account_id}</div>
				{isTelegram && ch.account_id && (
					<a
						href={`https://t.me/${ch.account_id}`}
						target="_blank"
						className="text-xs text-[var(--accent)] underline"
						rel="noopener"
					>
						t.me/{ch.account_id}
					</a>
				)}
				<ChannelPolicyFields config={cfg} />
				<ChannelModelFields model={editModel} reasoningEffort={editReasoningEffort} />
				<label>
					<span className="text-xs text-[var(--muted)]">Agent</span>
					<select
						className="channel-select"
						value={editAgent.value}
						onChange={(e: Event) => {
							editAgent.value = targetValue(e);
						}}
					>
						<option value="">(default agent)</option>
						{agentsList.value.map((a) => (
							<option key={a.id} value={a.id}>
								{a.emoji ? `${a.emoji} ` : ""}
								{a.name}
							</option>
						))}
					</select>
				</label>
				<ChannelAllowlistFields allowlist={allowlistItems} />
				<AdvancedConfigPatchField
					value={editAdvancedConfigPatch.value}
					onInput={(value) => {
						editAdvancedConfigPatch.value = value;
					}}
					currentConfig={cfg}
				/>
				{error.value && <div className="text-xs text-[var(--error)] py-1">{error.value}</div>}
				<button type="button" className="provider-btn" onClick={onSave} disabled={saving.value}>
					{saving.value ? "Saving\u2026" : "Save Changes"}
				</button>
			</div>
		</Modal>
	);
}
