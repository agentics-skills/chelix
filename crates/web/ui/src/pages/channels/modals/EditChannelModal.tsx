// ── Edit channel modal ───────────────────────────────────────

import { type Signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { useEffect } from "preact/hooks";

import { parseChannelConfigPatch } from "../../../channel-utils";
import { sendRpc } from "../../../helpers";
import { targetValue } from "../../../typed-events";
import { ChannelType } from "../../../types/channel";
import { Modal } from "../../../ui";
import { type ChannelConfig, channelLabel, channelType, editingChannel, loadChannels } from "../../ChannelsPage";
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
	roomAllowlist: string[];
	signalAccount: string;
	signalHttpUrl: string;
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
		roomAllowlist: firstConfigArray([config.group_allowlist]),
		signalAccount: configString(config.account),
		signalHttpUrl: configString(config.http_url, "http://127.0.0.1:8080"),
	};
}

function formFieldValue(form: HTMLElement, field: string, fallback: string): string {
	return (
		(form.querySelector(`[data-field=${field}]`) as HTMLInputElement | HTMLSelectElement | null)?.value || fallback
	);
}

function applyChannelCredentials(
	config: ChannelConfig,
	channelKind: string,
	current: ChannelConfig,
	draft: EditChannelDraft,
): void {
	if (channelKind === ChannelType.Telegram) config.token = current.token || "";
	if (channelKind === ChannelType.Signal) {
		config.account = draft.signalAccount.trim();
		config.http_url = draft.signalHttpUrl.trim() || "http://127.0.0.1:8080";
	}
}

function applySignalPolicy(
	config: ChannelConfig,
	current: ChannelConfig,
	draft: EditChannelDraft,
	form: HTMLElement,
): void {
	config.group_policy = formFieldValue(form, "groupPolicy", configString(current.group_policy, "disabled"));
	config.group_allowlist = draft.roomAllowlist;
	config.otp_self_approval = current.otp_self_approval !== false;
	config.otp_cooldown_secs = current.otp_cooldown_secs ?? 300;
	config.ignore_stories = current.ignore_stories !== false;
	config.text_chunk_limit = (current.text_chunk_limit as number) || 4000;
	if (current.account_uuid) config.account_uuid = current.account_uuid as string;
}

function buildChannelUpdateConfig(
	form: HTMLElement,
	channelKind: string,
	current: ChannelConfig,
	draft: EditChannelDraft,
): ChannelConfig {
	const isWhatsApp = channelKind === ChannelType.WhatsApp;
	const config: ChannelConfig = {
		dm_policy: formFieldValue(form, "dmPolicy", isWhatsApp ? "open" : "allowlist"),
		allowlist: draft.allowlist,
		agent_id: draft.agent || null,
	};
	if (channelKind === ChannelType.Signal) applySignalPolicy(config, current, draft, form);
	if (!isWhatsApp) config.mention_mode = formFieldValue(form, "mentionMode", "mention");
	applyChannelCredentials(config, channelKind, current, draft);
	return config;
}

interface SignalEditFieldsProps {
	account: Signal<string>;
	httpUrl: Signal<string>;
}

function SignalEditFields({ account, httpUrl }: SignalEditFieldsProps): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">Signal Account</span>
				<input
					type="text"
					className="channel-input w-full"
					value={account.value}
					onInput={(event) => {
						account.value = targetValue(event);
					}}
					placeholder="+15551234567"
				/>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">signal-cli Daemon URL</span>
				<input
					type="url"
					className="channel-input w-full"
					value={httpUrl.value}
					onInput={(event) => {
						httpUrl.value = targetValue(event);
					}}
					placeholder="http://127.0.0.1:8080"
				/>
			</label>
		</>
	);
}

interface ChannelPolicyFieldsProps {
	config: ChannelConfig;
	isWhatsApp: boolean;
	isSignal: boolean;
}

function ChannelPolicyFields({ config, isWhatsApp, isSignal }: ChannelPolicyFieldsProps): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">DM Policy</span>
				<select
					data-field="dmPolicy"
					className="channel-select"
					value={configString(config.dm_policy, isWhatsApp ? "open" : "allowlist")}
				>
					{isWhatsApp ? <option value="open">Open (anyone)</option> : null}
					<option value="allowlist">Allowlist only</option>
					{isWhatsApp ? null : <option value="open">Open (anyone)</option>}
					<option value="disabled">Disabled</option>
				</select>
			</label>
			{isWhatsApp ? null : (
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
			)}
			{isSignal ? (
				<label>
					<span className="text-xs text-[var(--muted)]">Group Policy</span>
					<select
						data-field="groupPolicy"
						className="channel-select"
						value={configString(config.group_policy, "disabled")}
					>
						<option value="disabled">Disabled</option>
						<option value="allowlist">Allowlist only</option>
						<option value="open">Open (any group)</option>
					</select>
				</label>
			) : null}
		</>
	);
}

interface ChannelAllowlistFieldsProps {
	isSignal: boolean;
	allowlist: Signal<string[]>;
	roomAllowlist: Signal<string[]>;
}

function ChannelAllowlistFields({ isSignal, allowlist, roomAllowlist }: ChannelAllowlistFieldsProps): VNode {
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
			{isSignal ? (
				<>
					<span className="text-xs text-[var(--muted)]">Group Allowlist</span>
					<AllowlistInput
						ariaLabel="Group Allowlist"
						value={roomAllowlist.value}
						onChange={(value) => {
							roomAllowlist.value = value;
						}}
					/>
				</>
			) : null}
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
	const roomAllowlistItems = useSignal<string[]>([]);
	const editSignalAccount = useSignal("");
	const editSignalHttpUrl = useSignal("http://127.0.0.1:8080");
	const editAdvancedConfigPatch = useSignal("");

	useEffect(() => {
		const draft = channelEditDraft(ch?.config || {});
		editModel.value = draft.model;
		editReasoningEffort.value = draft.reasoningEffort;
		editAgent.value = draft.agent;
		allowlistItems.value = draft.allowlist;
		roomAllowlistItems.value = draft.roomAllowlist;
		editSignalAccount.value = draft.signalAccount;
		editSignalHttpUrl.value = draft.signalHttpUrl;
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
	const isWhatsApp = chType === ChannelType.WhatsApp;
	const isTelegram = chType === ChannelType.Telegram;
	const isSignal = chType === ChannelType.Signal;

	function currentDraft(): EditChannelDraft {
		return {
			model: editModel.value,
			reasoningEffort: editReasoningEffort.value,
			agent: editAgent.value,
			allowlist: allowlistItems.value,
			roomAllowlist: roomAllowlistItems.value,
			signalAccount: editSignalAccount.value,
			signalHttpUrl: editSignalHttpUrl.value,
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
			title={`Edit ${channelLabel(ch.type)} Channel`}
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
				{isSignal ? <SignalEditFields account={editSignalAccount} httpUrl={editSignalHttpUrl} /> : null}
				<ChannelPolicyFields config={cfg} isWhatsApp={isWhatsApp} isSignal={isSignal} />
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
				<ChannelAllowlistFields isSignal={isSignal} allowlist={allowlistItems} roomAllowlist={roomAllowlistItems} />
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
