// ── Edit channel modal ───────────────────────────────────────

import { type Signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { useEffect } from "preact/hooks";

import {
	MATRIX_DOCS_URL,
	MATRIX_ENCRYPTION_GUIDANCE,
	matrixAuthModeGuidance,
	matrixCredentialLabel,
	matrixCredentialPlaceholder,
	matrixOwnershipModeGuidance,
	normalizeMatrixAuthMode,
	normalizeMatrixOtpCooldown,
	normalizeMatrixOwnershipMode,
	parseChannelConfigPatch,
} from "../../../channel-utils";
import { sendRpc } from "../../../helpers";
import { models as modelsSig } from "../../../stores/model-store";
import { targetChecked, targetValue } from "../../../typed-events";
import { ChannelType } from "../../../types/channel";
import { Modal, ModelSelect } from "../../../ui";
import { type ChannelConfig, channelLabel, channelType, editingChannel, loadChannels } from "../../ChannelsPage";
import { AdvancedConfigPatchField, AllowlistInput } from "../ChannelFields";

interface EditChannelDraft {
	model: string;
	agent: string;
	allowlist: string[];
	roomAllowlist: string[];
	credential: string;
	matrixAuthMode: string;
	matrixDeviceDisplayName: string;
	matrixOwnershipMode: string;
	matrixOtpSelfApproval: boolean;
	matrixOtpCooldown: string;
	signalAccount: string;
	signalHttpUrl: string;
	channelNamePatterns: string[];
	categoryAllowlist: string[];
}

function configString(value: unknown, fallback = ""): string {
	return typeof value === "string" && value ? value : fallback;
}

function firstConfigArray(values: unknown[]): string[] {
	return (values.find(Array.isArray) as string[] | undefined) || [];
}

function channelEditDraft(config: ChannelConfig): EditChannelDraft {
	const usesPassword = Boolean(config.password);
	return {
		model: configString(config.model),
		agent: configString(config.agent_id),
		allowlist: firstConfigArray([config.allowlist, config.user_allowlist, config.allowed_pubkeys]),
		roomAllowlist: firstConfigArray([config.room_allowlist, config.group_allowlist]),
		credential: "",
		matrixAuthMode: usesPassword ? "password" : "access_token",
		matrixDeviceDisplayName: configString(config.device_display_name),
		matrixOwnershipMode: normalizeMatrixOwnershipMode(
			configString(config.ownership_mode, usesPassword ? "chelix_owned" : "user_managed"),
		),
		matrixOtpSelfApproval: config.otp_self_approval !== false,
		matrixOtpCooldown: String(config.otp_cooldown_secs || 300),
		signalAccount: configString(config.account),
		signalHttpUrl: configString(config.http_url, "http://127.0.0.1:8080"),
		channelNamePatterns: firstConfigArray([config.channel_name_patterns]),
		categoryAllowlist: firstConfigArray([config.category_allowlist]),
	};
}

function formFieldValue(form: HTMLElement, field: string, fallback: string): string {
	return (
		(form.querySelector(`[data-field=${field}]`) as HTMLInputElement | HTMLSelectElement | null)?.value || fallback
	);
}

function applySelectedModel(config: ChannelConfig, modelId: string): void {
	if (!modelId) return;
	config.model = modelId;
	const provider = modelsSig.value.find((model) => model.id === modelId)?.provider;
	if (provider) config.model_provider = provider;
}

function applyMatrixCredentials(
	config: ChannelConfig,
	current: ChannelConfig,
	draft: EditChannelDraft,
	form: HTMLElement,
): void {
	config.homeserver = formFieldValue(form, "homeserver", configString(current.homeserver));
	config.user_id = formFieldValue(form, "userId", configString(current.user_id));
	config.device_id = current.device_id || undefined;
	config.device_display_name = draft.matrixDeviceDisplayName.trim() || null;
	const authMode = normalizeMatrixAuthMode(draft.matrixAuthMode);
	config.ownership_mode =
		authMode === "password" ? normalizeMatrixOwnershipMode(draft.matrixOwnershipMode) : "user_managed";
	if (authMode === "password") {
		config.password = draft.credential || current.password || "";
		config.access_token = "";
		return;
	}
	config.access_token = draft.credential || current.access_token || "";
	config.password = null;
}

function applyChannelCredentials(
	config: ChannelConfig,
	channelKind: string,
	current: ChannelConfig,
	draft: EditChannelDraft,
	form: HTMLElement,
): void {
	if (channelKind === ChannelType.Discord) config.token = draft.credential || current.token || "";
	if (channelKind === ChannelType.Telegram) config.token = current.token || "";
	if (channelKind === ChannelType.Signal) {
		config.account = draft.signalAccount.trim();
		config.http_url = draft.signalHttpUrl.trim() || "http://127.0.0.1:8080";
	}
	if (channelKind === ChannelType.Matrix) applyMatrixCredentials(config, current, draft, form);
}

function applyMatrixPolicy(
	config: ChannelConfig,
	current: ChannelConfig,
	draft: EditChannelDraft,
	form: HTMLElement,
): void {
	config.user_allowlist = draft.allowlist;
	config.room_policy = formFieldValue(form, "roomPolicy", configString(current.room_policy, "allowlist"));
	config.auto_join = formFieldValue(form, "autoJoin", configString(current.auto_join, "always"));
	config.room_allowlist = draft.roomAllowlist;
	config.otp_self_approval = draft.matrixOtpSelfApproval;
	config.otp_cooldown_secs = normalizeMatrixOtpCooldown(draft.matrixOtpCooldown);
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
	if (channelKind === ChannelType.Matrix) applyMatrixPolicy(config, current, draft, form);
	if (channelKind === ChannelType.Signal) applySignalPolicy(config, current, draft, form);
	if (!isWhatsApp) config.mention_mode = formFieldValue(form, "mentionMode", "mention");
	if (channelKind === ChannelType.Discord) {
		config.channel_name_patterns = draft.channelNamePatterns;
		config.category_allowlist = draft.categoryAllowlist;
	}
	applyChannelCredentials(config, channelKind, current, draft, form);
	applySelectedModel(config, draft.model);
	return config;
}

interface DiscordEditFieldsProps {
	credential: Signal<string>;
	channelNamePatterns: Signal<string[]>;
	categoryAllowlist: Signal<string[]>;
}

function DiscordEditFields({ credential, channelNamePatterns, categoryAllowlist }: DiscordEditFieldsProps): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">Bot Token (optional: leave blank to keep existing)</span>
				<input
					type="password"
					className="channel-input w-full"
					value={credential.value}
					onInput={(event) => {
						credential.value = targetValue(event);
					}}
				/>
			</label>
			<span className="text-xs text-[var(--muted)]">Channel Name Patterns (optional)</span>
			<AllowlistInput
				ariaLabel="Channel Name Patterns (optional)"
				value={channelNamePatterns.value}
				onChange={(value) => {
					channelNamePatterns.value = value;
				}}
				placeholder="e.g. ticket-* (glob patterns, Enter to add)"
			/>
			<div className="text-xs text-[var(--muted)] -mt-1">
				When set, the bot only responds in guild channels whose name matches a pattern. Matched channels do not require
				@mention. Supports * wildcards.
			</div>
			<span className="text-xs text-[var(--muted)]">Category IDs (optional)</span>
			<AllowlistInput
				ariaLabel="Category IDs (optional)"
				value={categoryAllowlist.value}
				onChange={(value) => {
					categoryAllowlist.value = value;
				}}
				placeholder="Discord category ID (Enter to add)"
			/>
			<div className="text-xs text-[var(--muted)] -mt-1">
				Only respond in channels under these Discord categories. Combined with name patterns via OR.
			</div>
		</>
	);
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

interface MatrixEditFieldsProps {
	config: ChannelConfig;
	authMode: Signal<string>;
	ownershipMode: Signal<string>;
	credential: Signal<string>;
	deviceDisplayName: Signal<string>;
}

function MatrixOwnershipField({
	authMode,
	ownershipMode,
}: Pick<MatrixEditFieldsProps, "authMode" | "ownershipMode">): VNode {
	if (authMode.value !== "password") {
		return (
			<div className="text-xs text-[var(--muted)]">{matrixOwnershipModeGuidance(authMode.value, "user_managed")}</div>
		);
	}
	return (
		<label className="flex items-start gap-2 rounded-md border border-[var(--border)] bg-[var(--surface2)] px-3 py-2">
			<input
				type="checkbox"
				aria-label="Let Chelix own this Matrix account"
				checked={normalizeMatrixOwnershipMode(ownershipMode.value) === "chelix_owned"}
				onChange={(event) => {
					ownershipMode.value = targetChecked(event) ? "chelix_owned" : "user_managed";
				}}
			/>
			<span className="flex flex-col gap-1">
				<span className="text-xs font-medium text-[var(--text-strong)]">Let Chelix own this Matrix account</span>
				<span className="text-xs text-[var(--muted)]">
					{matrixOwnershipModeGuidance(authMode.value, ownershipMode.value)}
				</span>
			</span>
		</label>
	);
}

function MatrixCredentialGuidance({ authMode }: { authMode: string }): VNode {
	return (
		<div className="text-xs text-[var(--muted)]">
			{authMode === "password" ? (
				"Password auth is required for encrypted Matrix chats because Chelix needs its own Matrix device keys."
			) : (
				<>
					Access token mode does <span className="font-medium">not</span> support encrypted Matrix chats because Chelix
					cannot import the existing device's private encryption keys.
				</>
			)}{" "}
			<a href={MATRIX_DOCS_URL} target="_blank" rel="noreferrer" className="text-[var(--accent)] underline">
				Matrix setup docs
			</a>
		</div>
	);
}

function MatrixEditFields({
	config,
	authMode,
	ownershipMode,
	credential,
	deviceDisplayName,
}: MatrixEditFieldsProps): VNode {
	return (
		<>
			<div className="rounded-md border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-100">
				<div className="font-medium text-emerald-50">Encrypted chats require password auth</div>
				<div>{MATRIX_ENCRYPTION_GUIDANCE}</div>
			</div>
			<label>
				<span className="text-xs text-[var(--muted)]">Authentication</span>
				<select
					className="channel-select w-full"
					value={authMode.value}
					onChange={(event) => {
						authMode.value = normalizeMatrixAuthMode(targetValue(event));
					}}
				>
					<option value="access_token">Access token</option>
					<option value="password">Password</option>
				</select>
			</label>
			<div className="text-xs text-[var(--muted)]">{matrixAuthModeGuidance(authMode.value)}</div>
			<MatrixOwnershipField authMode={authMode} ownershipMode={ownershipMode} />
			<label>
				<span className="text-xs text-[var(--muted)]">Homeserver URL</span>
				<input
					data-field="homeserver"
					type="text"
					className="channel-input w-full"
					defaultValue={configString(config.homeserver)}
				/>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">
					Matrix User ID{authMode.value === "password" ? " (required)" : " (optional)"}
				</span>
				<input
					data-field="userId"
					type="text"
					className="channel-input w-full"
					defaultValue={configString(config.user_id)}
				/>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">
					{matrixCredentialLabel(authMode.value)} (optional: leave blank to keep existing)
				</span>
				<input
					type="password"
					className="channel-input w-full"
					value={credential.value}
					onInput={(event) => {
						credential.value = targetValue(event);
					}}
					placeholder={matrixCredentialPlaceholder(authMode.value)}
				/>
			</label>
			<MatrixCredentialGuidance authMode={authMode.value} />
			<label>
				<span className="text-xs text-[var(--muted)]">Device Display Name (optional)</span>
				<input
					type="text"
					className="channel-input w-full"
					value={deviceDisplayName.value}
					onInput={(event) => {
						deviceDisplayName.value = targetValue(event);
					}}
				/>
			</label>
		</>
	);
}

interface ChannelPolicyFieldsProps {
	config: ChannelConfig;
	isWhatsApp: boolean;
	isMatrix: boolean;
	isSignal: boolean;
	matrixOtpSelfApproval: Signal<boolean>;
	matrixOtpCooldown: Signal<string>;
}

function MatrixPolicyFields({
	config,
	otpSelfApproval,
	otpCooldown,
}: {
	config: ChannelConfig;
	otpSelfApproval: Signal<boolean>;
	otpCooldown: Signal<string>;
}): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">Unknown DM Approval</span>
				<select
					className="channel-select"
					value={otpSelfApproval.value ? "on" : "off"}
					onChange={(event) => {
						otpSelfApproval.value = targetValue(event) !== "off";
					}}
				>
					<option value="on">PIN challenge enabled (recommended)</option>
					<option value="off">Reject unknown DMs without a PIN</option>
				</select>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">PIN Cooldown Seconds</span>
				<input
					type="number"
					min={1}
					step={1}
					className="channel-input"
					value={otpCooldown.value}
					onInput={(event) => {
						otpCooldown.value = targetValue(event);
					}}
				/>
			</label>
			<div className="text-xs text-[var(--muted)]">
				With DM policy on allowlist, unknown users get a 6-digit PIN challenge by default.
			</div>
			<label>
				<span className="text-xs text-[var(--muted)]">Room Policy</span>
				<select
					data-field="roomPolicy"
					className="channel-select"
					value={configString(config.room_policy, "allowlist")}
				>
					<option value="allowlist">Room allowlist only</option>
					<option value="open">Open (any joined room)</option>
					<option value="disabled">Disabled</option>
				</select>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)]">Invite Auto-Join</span>
				<select data-field="autoJoin" className="channel-select" value={configString(config.auto_join, "always")}>
					<option value="always">Always join invites</option>
					<option value="allowlist">Only when inviter or room is allowlisted</option>
					<option value="off">Do not auto-join</option>
				</select>
			</label>
		</>
	);
}

function ChannelPolicyFields({
	config,
	isWhatsApp,
	isMatrix,
	isSignal,
	matrixOtpSelfApproval,
	matrixOtpCooldown,
}: ChannelPolicyFieldsProps): VNode {
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
			{isMatrix ? (
				<MatrixPolicyFields config={config} otpSelfApproval={matrixOtpSelfApproval} otpCooldown={matrixOtpCooldown} />
			) : null}
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
	isMatrix: boolean;
	isSignal: boolean;
	allowlist: Signal<string[]>;
	roomAllowlist: Signal<string[]>;
}

function ChannelAllowlistFields({ isMatrix, isSignal, allowlist, roomAllowlist }: ChannelAllowlistFieldsProps): VNode {
	return (
		<>
			<span className="text-xs text-[var(--muted)]">DM Allowlist</span>
			<AllowlistInput
				ariaLabel="DM Allowlist"
				value={allowlist.value}
				preserveAt={isMatrix}
				onChange={(value) => {
					allowlist.value = value;
				}}
			/>
			{isMatrix ? (
				<>
					<span className="text-xs text-[var(--muted)]">Room Allowlist</span>
					<AllowlistInput
						ariaLabel="Room Allowlist"
						value={roomAllowlist.value}
						preserveAt={true}
						onChange={(value) => {
							roomAllowlist.value = value;
						}}
					/>
				</>
			) : null}
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
	const editAgent = useSignal("");
	const agentsList = useSignal<Array<{ id: string; name: string; emoji?: string }>>([]);
	const allowlistItems = useSignal<string[]>([]);
	const roomAllowlistItems = useSignal<string[]>([]);
	const editCredential = useSignal("");
	const editMatrixAuthMode = useSignal("access_token");
	const editMatrixDeviceDisplayName = useSignal("");
	const editMatrixOwnershipMode = useSignal("user_managed");
	const editMatrixOtpSelfApproval = useSignal(true);
	const editMatrixOtpCooldown = useSignal("300");
	const editSignalAccount = useSignal("");
	const editSignalHttpUrl = useSignal("http://127.0.0.1:8080");
	const editChannelNamePatterns = useSignal<string[]>([]);
	const editCategoryAllowlist = useSignal<string[]>([]);
	const editAdvancedConfigPatch = useSignal("");

	useEffect(() => {
		const draft = channelEditDraft(ch?.config || {});
		editModel.value = draft.model;
		editAgent.value = draft.agent;
		allowlistItems.value = draft.allowlist;
		roomAllowlistItems.value = draft.roomAllowlist;
		editCredential.value = draft.credential;
		editMatrixAuthMode.value = draft.matrixAuthMode;
		editMatrixDeviceDisplayName.value = draft.matrixDeviceDisplayName;
		editMatrixOwnershipMode.value = draft.matrixOwnershipMode;
		editMatrixOtpSelfApproval.value = draft.matrixOtpSelfApproval;
		editMatrixOtpCooldown.value = draft.matrixOtpCooldown;
		editSignalAccount.value = draft.signalAccount;
		editSignalHttpUrl.value = draft.signalHttpUrl;
		editChannelNamePatterns.value = draft.channelNamePatterns;
		editCategoryAllowlist.value = draft.categoryAllowlist;
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
	const isDiscord = chType === ChannelType.Discord;
	const isWhatsApp = chType === ChannelType.WhatsApp;
	const isTelegram = chType === ChannelType.Telegram;
	const isMatrix = chType === ChannelType.Matrix;
	const isSignal = chType === ChannelType.Signal;

	function currentDraft(): EditChannelDraft {
		return {
			model: editModel.value,
			agent: editAgent.value,
			allowlist: allowlistItems.value,
			roomAllowlist: roomAllowlistItems.value,
			credential: editCredential.value,
			matrixAuthMode: editMatrixAuthMode.value,
			matrixDeviceDisplayName: editMatrixDeviceDisplayName.value,
			matrixOwnershipMode: editMatrixOwnershipMode.value,
			matrixOtpSelfApproval: editMatrixOtpSelfApproval.value,
			matrixOtpCooldown: editMatrixOtpCooldown.value,
			signalAccount: editSignalAccount.value,
			signalHttpUrl: editSignalHttpUrl.value,
			channelNamePatterns: editChannelNamePatterns.value,
			categoryAllowlist: editCategoryAllowlist.value,
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
		error.value = "";
		if (!ch) return;
		saving.value = true;
		const updateConfig = buildChannelUpdateConfig(form, chType, cfg, currentDraft());
		Object.assign(updateConfig, advancedPatch.value);
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

	const defaultPlaceholder =
		modelsSig.value.length > 0
			? `(default: ${modelsSig.value[0].display_name || modelsSig.value[0].id})`
			: "(server default)";

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
				{isDiscord ? (
					<DiscordEditFields
						credential={editCredential}
						channelNamePatterns={editChannelNamePatterns}
						categoryAllowlist={editCategoryAllowlist}
					/>
				) : null}
				{isSignal ? <SignalEditFields account={editSignalAccount} httpUrl={editSignalHttpUrl} /> : null}
				{isMatrix ? (
					<MatrixEditFields
						config={cfg}
						authMode={editMatrixAuthMode}
						ownershipMode={editMatrixOwnershipMode}
						credential={editCredential}
						deviceDisplayName={editMatrixDeviceDisplayName}
					/>
				) : null}
				<ChannelPolicyFields
					config={cfg}
					isWhatsApp={isWhatsApp}
					isMatrix={isMatrix}
					isSignal={isSignal}
					matrixOtpSelfApproval={editMatrixOtpSelfApproval}
					matrixOtpCooldown={editMatrixOtpCooldown}
				/>
				<span className="text-xs text-[var(--muted)]">Default Model</span>
				<ModelSelect
					ariaLabel="Default Model"
					models={modelsSig.value}
					value={editModel.value}
					onChange={(v: string) => {
						editModel.value = v;
					}}
					placeholder={defaultPlaceholder}
				/>
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
				<ChannelAllowlistFields
					isMatrix={isMatrix}
					isSignal={isSignal}
					allowlist={allowlistItems}
					roomAllowlist={roomAllowlistItems}
				/>
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
