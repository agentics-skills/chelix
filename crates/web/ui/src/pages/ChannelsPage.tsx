// ── Channels page (Preact + Signals) ──────────────────────────

import type { Signal } from "@preact/signals";
import { computed, signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect } from "preact/hooks";
import {
	channelStorageNote,
	fetchChannelStatus,
	matrixOwnershipModeGuidance,
	normalizeMatrixAuthMode,
	normalizeMatrixOwnershipMode,
} from "../channel-utils";
import { TabBar } from "../components/forms/Tabs";
import { onEvent } from "../events";
import { get as getGon } from "../gon";
import { sendRpc } from "../helpers";
import { updateNavCount } from "../nav-counts";
import { connected } from "../signals";
import * as S from "../state";
import { ConfirmDialog, copyToClipboard, requestConfirm, showToast } from "../ui";
import { AddDiscordModal } from "./channels/modals/AddDiscordModal";
import { AddMatrixModal } from "./channels/modals/AddMatrixModal";
import { AddSignalModal } from "./channels/modals/AddSignalModal";
import { AddSlackModal } from "./channels/modals/AddSlackModal";
// ── Sub-module imports (modals + shared fields) ──────────────
import { AddTelegramModal } from "./channels/modals/AddTelegramModal";
import { AddWhatsAppModal } from "./channels/modals/AddWhatsAppModal";
import { EditChannelModal } from "./channels/modals/EditChannelModal";

// ── Types ────────────────────────────────────────────────────

interface ChannelSession {
	key: string;
	label?: string;
	active?: boolean;
	messageCount?: number;
}

interface MatrixVerificationPrompt {
	other_user_id: string;
}

interface MatrixStatus {
	user_id?: string;
	device_id?: string;
	device_display_name?: string;
	ownership_mode?: string;
	auth_mode?: string;
	recovery_state?: string;
	device_verified_by_owner?: boolean;
	ownership_error?: string;
	cross_signing_complete?: boolean;
	verification_state?: string;
	pending_verifications?: MatrixVerificationPrompt[];
}

interface ChannelExtra {
	matrix?: MatrixStatus;
	qr_data?: string;
	qr_svg?: string;
}

/** Channel config fields (union of all channel types). */
export interface ChannelConfig {
	// Common
	token?: string;
	dm_policy?: string;
	mention_mode?: string;
	allowlist?: string[];
	model?: string;
	model_provider?: string;
	// Slack
	bot_token?: string;
	app_token?: string;
	connection_mode?: string;
	group_policy?: string;
	signing_secret?: string;
	channel_allowlist?: string[];
	// Matrix
	homeserver?: string;
	user_id?: string;
	password?: string | null;
	access_token?: string;
	device_id?: string;
	device_display_name?: string | null;
	ownership_mode?: string;
	room_policy?: string;
	auto_join?: string;
	user_allowlist?: string[];
	room_allowlist?: string[];
	otp_self_approval?: boolean;
	otp_cooldown_secs?: number;
	// Signal
	account?: string;
	account_uuid?: string;
	http_url?: string;
	group_allowlist?: string[];
	text_chunk_limit?: number;
	// Advanced config patch pass-through
	[key: string]: unknown;
}

export interface Channel {
	type: string;
	account_id: string;
	name?: string;
	details?: string;
	status?: string;
	config?: ChannelConfig;
	sessions?: ChannelSession[];
	extra?: ChannelExtra;
}

interface ChannelDescriptor {
	channel_type: string;
	capabilities: {
		inbound_mode: string;
	};
}

interface SenderEntry {
	peer_id: string;
	sender_name?: string;
	username?: string;
	message_count: number;
	last_seen?: number;
	allowed?: boolean;
	otp_pending?: { code: string };
}

interface ChannelEvent {
	kind: string;
	account_id?: string;
	channel_type?: string;
	qr_data?: string;
	qr_svg?: string;
	reason?: string;
}

// ── Module-level signals ─────────────────────────────────────

const channels: Signal<Channel[]> = signal([]);

export function prefetchChannels(): Promise<void> {
	return fetchChannelStatus().then((res: unknown) => {
		const r = res as { ok?: boolean; payload?: { channels?: Channel[] } } | undefined;
		if (r?.ok) {
			const ch = r.payload?.channels || [];
			channels.value = ch;
			S.setCachedChannels(ch);
		}
	});
}

const senders: Signal<SenderEntry[]> = signal([]);
const activeTab: Signal<string> = signal("channels");
export const showAddTelegram: Signal<boolean> = signal(false);
export const showAddDiscord: Signal<boolean> = signal(false);
export const showAddWhatsApp: Signal<boolean> = signal(false);
export const showAddSlack: Signal<boolean> = signal(false);
export const showAddMatrix: Signal<boolean> = signal(false);
export const showAddSignal: Signal<boolean> = signal(false);
export const editingChannel: Signal<Channel | null> = signal(null);
const sendersAccount: Signal<string> = signal("");

// Track WhatsApp pairing state (updated by WebSocket events).
export const waQrData: Signal<string | null> = signal(null);
export const waQrSvg: Signal<string | null> = signal(null);
export const waPairingAccountId: Signal<string | null> = signal(null);
export const waPairingError: Signal<string | null> = signal(null);

// ── Helpers ──────────────────────────────────────────────────

export function channelType(type: string | undefined): string {
	return type || "telegram";
}

export function channelLabel(type: string | undefined): string {
	const t = channelType(type);
	if (t === "discord") return "Discord";
	if (t === "whatsapp") return "WhatsApp";
	if (t === "slack") return "Slack";
	if (t === "matrix") return "Matrix";
	if (t === "signal") return "Signal";
	return "Telegram";
}

function channelDescriptor(type: string | undefined): ChannelDescriptor | null {
	const descs = (getGon("channel_descriptors") || []) as ChannelDescriptor[];
	return descs.find((d) => d.channel_type === channelType(type)) || null;
}

const MODE_LABELS: Record<string, string> = {
	none: "Send only",
	polling: "Polling",
	gateway_loop: "Gateway",
	socket_mode: "Socket Mode",
	webhook: "Webhook",
};

const MODE_HINTS: Record<string, string> = {
	webhook: "Requires a publicly reachable URL. Configure your platform to send events to the endpoint shown below.",
	polling: "Connects automatically via long-polling. No public URL needed.",
	gateway_loop: "Maintains a persistent connection. No public URL needed.",
	socket_mode: "Connects via Socket Mode. No public URL needed.",
	none: "This channel is send-only and cannot receive inbound messages.",
};

// ── Small sub-components ─────────────────────────────────────

interface ConnectionModeHintProps {
	type: string;
}

export function ConnectionModeHint({ type }: ConnectionModeHintProps): VNode | null {
	const desc = channelDescriptor(type);
	if (!desc) return null;
	const hint = MODE_HINTS[desc.capabilities.inbound_mode];
	if (!hint) return null;
	return (
		<div className="text-xs text-[var(--muted)] mt-1 flex items-center gap-1">
			<span className="tier-badge">{MODE_LABELS[desc.capabilities.inbound_mode]}</span>
			<span>{hint}</span>
		</div>
	);
}

interface ChannelStorageNoticeProps {
	compact?: boolean;
}

function ChannelStorageNotice({ compact = false }: ChannelStorageNoticeProps): VNode {
	return (
		<div
			className={`rounded-md border border-[var(--border)] bg-[var(--surface2)] px-3 py-2 text-xs text-[var(--muted)] ${compact ? "" : "max-w-3xl"}`}
		>
			<span className="font-medium text-[var(--text-strong)]">Storage note.</span> {channelStorageNote()}
		</div>
	);
}

// ── Matrix info row ──────────────────────────────────────────

interface MatrixInfoRowProps {
	label: string;
	value: unknown;
	copyLabel?: string | null;
}

function MatrixInfoRow({ label, value, copyLabel = null }: MatrixInfoRowProps): VNode {
	const text = String(value || "").trim();
	return (
		<div className="flex items-center justify-between gap-3">
			<div className="min-w-0">
				<div className="text-[11px] uppercase tracking-wide text-sky-700">{label}</div>
				<div className="truncate font-mono text-sky-900">{text || "\u2014"}</div>
			</div>
			{text && (
				<button
					type="button"
					className="provider-btn provider-btn-sm provider-btn-secondary"
					onClick={() => copyToClipboard(text, copyLabel || `${label} copied`)}
				>
					Copy
				</button>
			)}
		</div>
	);
}

// ── Matrix ownership card ────────────────────────────────────

interface MatrixOwnershipCardProps {
	channel: Channel;
	matrixStatus: MatrixStatus;
}

type MatrixOwnershipIssue = "none" | "approval_required" | "incomplete_secret_storage" | "generic_blocked";

interface MatrixOwnershipView {
	issue: MatrixOwnershipIssue;
	modeTitle: string;
	modeText: string;
	detailTitle: string;
	detailText: string;
	approvalUrl: string;
	verificationText: string;
	hasAccountDetails: boolean;
}

function matrixOwnershipIssue(ownershipMode: string, ownershipError: string): MatrixOwnershipIssue {
	if (ownershipMode !== "chelix_owned" || !ownershipError) return "none";
	if (ownershipError.includes("requires browser approval to reset cross-signing")) return "approval_required";
	if (ownershipError.includes("incomplete secret storage")) return "incomplete_secret_storage";
	return "generic_blocked";
}

function matrixOwnershipTitle(issue: MatrixOwnershipIssue, ownershipMode: string): string {
	if (issue === "approval_required") return "Ownership approval required";
	if (issue !== "none") return "Chelix ownership blocked";
	return ownershipMode === "chelix_owned" ? "Managed by Chelix" : "User-managed in Element";
}

function matrixOwnershipText(issue: MatrixOwnershipIssue, authMode: string, ownershipMode: string): string {
	if (issue === "approval_required") {
		return "This existing Matrix account can already chat, but Matrix needs one browser approval before Chelix can take over encryption ownership. Open the approval page, approve the reset, then retry ownership setup.";
	}
	if (issue === "incomplete_secret_storage") {
		return "This account already has partial Matrix secure-backup state. Finish or repair it in Element, or switch this channel to user-managed mode.";
	}
	if (issue === "generic_blocked") {
		return "Chelix could not take ownership of this Matrix account automatically. Repair the account in Element or switch this channel to user-managed mode.";
	}
	if (authMode === "password" || authMode === "oidc") return matrixOwnershipModeGuidance(authMode, ownershipMode);
	return "Access token auth is always user-managed. If you want encrypted Matrix chats, reconnect this channel with OIDC or password auth so Chelix can create its own device.";
}

function matrixOwnershipView(channel: Channel, matrixStatus: MatrixStatus): MatrixOwnershipView {
	const ownershipMode = normalizeMatrixOwnershipMode(matrixStatus.ownership_mode);
	const authMode = normalizeMatrixAuthMode(matrixStatus.auth_mode);
	const ownershipError = String(matrixStatus.ownership_error || "").trim();
	const issue = matrixOwnershipIssue(ownershipMode, ownershipError);
	const approvalMatch = ownershipError.match(/https?:\/\/\S+/);
	return {
		issue,
		modeTitle: matrixOwnershipTitle(issue, ownershipMode),
		modeText: matrixOwnershipText(issue, authMode, ownershipMode),
		detailTitle:
			issue === "approval_required"
				? "Browser approval pending"
				: ownershipError
					? "Ownership setup needs attention"
					: "",
		detailText:
			issue === "approval_required"
				? `Approve the reset while signed into ${matrixStatus.user_id || "this Matrix account"} in the browser, then use the retry button here so Chelix can finish taking ownership.`
				: ownershipError,
		approvalUrl: approvalMatch ? approvalMatch[0].replace(/[;),.]+$/, "") : "",
		verificationText: matrixStatus.device_verified_by_owner
			? "Device verified by owner"
			: "Device not yet verified by owner",
		hasAccountDetails: [
			channel.config?.homeserver,
			matrixStatus.user_id,
			matrixStatus.device_id,
			matrixStatus.device_display_name || channel.config?.device_display_name,
		].some((value) => !!String(value || "").trim()),
	};
}

function MatrixAccountDetails({ channel, status }: { channel: Channel; status: MatrixStatus }): VNode {
	return (
		<details className="mt-2 rounded-md border border-sky-600/20 bg-sky-100/50 px-3 py-2">
			<summary className="cursor-pointer text-[11px] font-medium uppercase tracking-wide text-sky-800">
				Matrix account details
			</summary>
			<div className="mt-2 grid gap-2">
				<MatrixInfoRow label="Homeserver" value={channel.config?.homeserver || ""} copyLabel="Homeserver copied" />
				<MatrixInfoRow label="Matrix user" value={status.user_id || ""} copyLabel="Matrix user ID copied" />
				<MatrixInfoRow label="Device ID" value={status.device_id || ""} copyLabel="Matrix device ID copied" />
				<MatrixInfoRow
					label="Device name"
					value={status.device_display_name || channel.config?.device_display_name || ""}
					copyLabel="Matrix device name copied"
				/>
			</div>
		</details>
	);
}

interface MatrixApprovalActionProps {
	show: boolean;
	approvalUrl: string;
	userId: string | undefined;
	retrying: boolean;
	error: string;
	onRetry: () => void;
}

function MatrixApprovalAction(props: MatrixApprovalActionProps): VNode | null {
	if (!(props.show && props.approvalUrl)) return null;
	const accountName = props.userId || "this account";
	return (
		<div className="mt-2">
			<div className="flex flex-wrap gap-2">
				<a
					href={props.approvalUrl}
					target="_blank"
					rel="noreferrer"
					className="provider-btn provider-btn-sm"
					aria-label={`Open approval page for ${props.userId || "this Matrix account"}`}
				>
					Open approval page for {accountName}
				</a>
				<button
					type="button"
					className="provider-btn provider-btn-sm"
					onClick={props.onRetry}
					disabled={props.retrying}
				>
					{props.retrying ? "Retrying ownership setup..." : "Click here once you reset the account"}
				</button>
			</div>
			<div className="mt-2 text-[11px] text-sky-800">
				Make sure the browser page is signed into{" "}
				<span className="font-mono text-sky-800">{props.userId || "the Matrix bot account"}</span>.
			</div>
			{props.error && (
				<div className="mt-2 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-amber-100">
					{props.error}
				</div>
			)}
		</div>
	);
}

function MatrixOwnershipDetail({ title, text }: { title: string; text: string }): VNode | null {
	if (!title) return null;
	return (
		<div className="mt-2 rounded-md border border-amber-500/30 bg-amber-500/10 px-3 py-2 text-amber-100">
			<div className="font-medium text-amber-50">{title}</div>
			<div className="mt-1">{text}</div>
		</div>
	);
}

function MatrixOwnershipCard({ channel, matrixStatus }: MatrixOwnershipCardProps): VNode {
	const retryingOwnership = useSignal(false);
	const retryOwnershipError = useSignal("");
	const view = matrixOwnershipView(channel, matrixStatus);
	const recoveryState = String(matrixStatus.recovery_state || "unknown");
	const deviceVerified = !!matrixStatus.device_verified_by_owner;

	function retryOwnershipSetup(): void {
		retryingOwnership.value = true;
		retryOwnershipError.value = "";
		sendRpc("channels.retry_ownership", {
			type: channelType(channel.type),
			account_id: channel.account_id,
		}).then((res) => {
			retryingOwnership.value = false;
			if (res?.ok) {
				showToast("Retrying Matrix ownership setup");
				loadChannels();
				return;
			}
			retryOwnershipError.value =
				(res?.error as { message?: string; detail?: string })?.message ||
				(res?.error as { detail?: string })?.detail ||
				"Failed to retry Matrix ownership setup.";
		});
	}

	return (
		<div className="rounded-md border border-sky-600/30 bg-sky-50 px-3 py-2 text-xs text-sky-900">
			<div className="flex items-center gap-2">
				<div className="font-medium text-sky-800">{view.modeTitle}</div>
				<span className={`provider-item-badge ${deviceVerified ? "configured" : "oauth"}`}>
					{view.verificationText}
				</span>
			</div>
			<div className="mt-1 text-sky-900">{view.modeText}</div>
			<div className="mt-2 text-sky-900">
				Cross-signing:{" "}
				<span className="font-medium">{matrixStatus?.cross_signing_complete ? "ready" : "not ready"}</span>. Recovery:{" "}
				<span className="font-medium">{recoveryState}</span>.
			</div>
			{view.hasAccountDetails && <MatrixAccountDetails channel={channel} status={matrixStatus} />}
			<MatrixApprovalAction
				show={view.issue === "approval_required"}
				approvalUrl={view.approvalUrl}
				userId={matrixStatus.user_id}
				retrying={retryingOwnership.value}
				error={retryOwnershipError.value}
				onRetry={retryOwnershipSetup}
			/>
			<MatrixOwnershipDetail title={view.detailTitle} text={view.detailText} />
		</div>
	);
}

// ── Sender selection helpers ─────────────────────────────────

function senderSelectionKey(ch: Channel): string {
	return `${channelType(ch.type)}::${ch.account_id}`;
}

function parseSenderSelectionKey(key: string): { type: string; account_id: string } {
	const idx = key.indexOf("::");
	if (idx < 0) return { type: "telegram", account_id: key };
	return {
		type: key.slice(0, idx) || "telegram",
		account_id: key.slice(idx + 2),
	};
}

// ── Data loaders ─────────────────────────────────────────────

export function loadChannels(): void {
	fetchChannelStatus().then((res: unknown) => {
		const r = res as { ok?: boolean; payload?: { channels?: Channel[] } } | undefined;
		if (r?.ok) {
			const ch = r.payload?.channels || [];
			channels.value = ch;
			S.setCachedChannels(ch);
			updateNavCount("channels", ch.length);
		}
	});
}

function loadSenders(): void {
	const selected = sendersAccount.value;
	if (!selected) {
		senders.value = [];
		return;
	}
	const parsed = parseSenderSelectionKey(selected);
	sendRpc<{ senders?: SenderEntry[] }>("channels.senders.list", {
		type: parsed.type,
		account_id: parsed.account_id,
	}).then((res) => {
		if (res?.ok) senders.value = (res.payload?.senders || []) as SenderEntry[];
	});
}

// ── Channel icon ─────────────────────────────────────────────

interface ChannelIconProps {
	type: string;
}

function ChannelIcon({ type }: ChannelIconProps): VNode {
	const t = channelType(type);
	if (t === "discord") return <span className="icon icon-discord" />;
	if (t === "whatsapp") return <span className="icon icon-whatsapp" />;
	if (t === "slack") return <span className="icon icon-slack" />;
	if (t === "matrix") return <span className="icon icon-matrix" />;
	return <span className="icon icon-telegram" />;
}

// ── Channel card ─────────────────────────────────────────────

interface ChannelCardProps {
	channel: Channel;
}

interface ChannelCardView {
	type: string;
	displayName: string;
	statusClass: string;
	sessionLine: string;
	modeLabel: string | null;
	matrixStatus: MatrixStatus | null;
	pendingVerifications: MatrixVerificationPrompt[];
	verificationStateLabel: string | null;
	showOwnershipCard: boolean;
	telegramUrl: string | null;
}

function channelSessionLine(sessions: ChannelSession[] | undefined): string {
	if (!sessions?.length) return "";
	const activeSessions = sessions.filter((session) => session.active);
	if (!activeSessions.length) return "No active session";
	return activeSessions.map((session) => `${session.label || session.key} (${session.messageCount} msgs)`).join(", ");
}

function channelCardView(channel: Channel): ChannelCardView {
	const type = channelType(channel.type);
	const descriptor = channelDescriptor(channel.type);
	const matrixStatus = channel.extra?.matrix || null;
	return {
		type,
		displayName: channel.name || channel.account_id || channelLabel(channel.type),
		statusClass: channel.status === "connected" ? "configured" : "oauth",
		sessionLine: channelSessionLine(channel.sessions),
		modeLabel: descriptor
			? MODE_LABELS[descriptor.capabilities.inbound_mode] || descriptor.capabilities.inbound_mode
			: null,
		matrixStatus,
		pendingVerifications: Array.isArray(matrixStatus?.pending_verifications) ? matrixStatus.pending_verifications : [],
		verificationStateLabel: matrixStatus?.verification_state || null,
		showOwnershipCard:
			type === "matrix" &&
			!!(
				matrixStatus?.user_id ||
				matrixStatus?.device_id ||
				channel.config?.homeserver ||
				matrixStatus?.ownership_error
			),
		telegramUrl: type === "telegram" && channel.account_id ? `https://t.me/${channel.account_id}` : null,
	};
}

function ChannelSummary({ channel, view }: { channel: Channel; view: ChannelCardView }): VNode {
	return (
		<div className="flex items-center gap-2.5">
			<span className="inline-flex items-center justify-center w-7 h-7 rounded-md bg-[var(--surface2)]">
				<ChannelIcon type={channel.type} />
			</span>
			<div className="flex flex-col gap-0.5">
				<span className="text-sm text-[var(--text-strong)]">{view.displayName}</span>
				{channel.details && <span className="text-xs text-[var(--muted)]">{channel.details}</span>}
				{view.sessionLine && <span className="text-xs text-[var(--muted)]">{view.sessionLine}</span>}
				{view.type === "matrix" && view.verificationStateLabel && (
					<span className="text-xs text-[var(--muted)]">Encryption device state: {view.verificationStateLabel}</span>
				)}
				{view.telegramUrl && (
					<a href={view.telegramUrl} target="_blank" className="text-xs text-[var(--accent)] underline">
						t.me/{channel.account_id}
					</a>
				)}
			</div>
			<span className={`provider-item-badge ${view.statusClass}`}>{channel.status || "unknown"}</span>
			{view.modeLabel && <span className="tier-badge">{view.modeLabel}</span>}
		</div>
	);
}

function MatrixVerificationPrompts({ prompts }: { prompts: MatrixVerificationPrompt[] }): VNode | null {
	if (!prompts.length) return null;
	return (
		<div className="rounded-md border border-emerald-500/30 bg-emerald-500/10 px-3 py-2 text-xs text-emerald-100">
			<div className="font-medium text-sky-900">Verification pending</div>
			{prompts.map((prompt) => (
				<div key={prompt.other_user_id} className="mt-1">
					<div>With {prompt.other_user_id}</div>
					<div className="text-emerald-200/90">
						Send <span className="font-mono">verify yes</span>, <span className="font-mono">verify no</span>,{" "}
						<span className="font-mono">verify show</span>, or <span className="font-mono">verify cancel</span> as a
						normal message in that same Matrix chat.
					</div>
				</div>
			))}
		</div>
	);
}

function ChannelCardActions({ channel, onRemove }: { channel: Channel; onRemove: () => void }): VNode {
	return (
		<div className="flex gap-2">
			<button
				type="button"
				className="provider-btn provider-btn-sm provider-btn-secondary"
				title={`Edit ${channel.account_id || "channel"}`}
				onClick={() => {
					editingChannel.value = channel;
				}}
			>
				Edit
			</button>
			<button
				type="button"
				className="provider-btn provider-btn-sm provider-btn-danger"
				title={`Remove ${channel.account_id || "channel"}`}
				onClick={onRemove}
			>
				Remove
			</button>
		</div>
	);
}

function ChannelCard({ channel: ch }: ChannelCardProps): VNode {
	function onRemove(): void {
		requestConfirm(`Remove ${ch.name || ch.account_id}?`).then((yes) => {
			if (!yes) return;
			sendRpc("channels.remove", { type: channelType(ch.type), account_id: ch.account_id }).then((r) => {
				if (r?.ok) loadChannels();
			});
		});
	}

	const view = channelCardView(ch);
	return (
		<div className="provider-card p-3 rounded-lg mb-2">
			<ChannelSummary channel={ch} view={view} />
			{view.type === "matrix" && <MatrixVerificationPrompts prompts={view.pendingVerifications} />}
			{view.showOwnershipCard && view.matrixStatus && (
				<MatrixOwnershipCard channel={ch} matrixStatus={view.matrixStatus} />
			)}
			<ChannelCardActions channel={ch} onRemove={onRemove} />
		</div>
	);
}

// ── Connect channel buttons ──────────────────────────────────

function ConnectButtons(): VNode {
	const offered = new Set(
		(getGon("channels_offered") || ["telegram", "whatsapp", "discord", "slack", "matrix"]) as string[],
	);
	return (
		<div className="flex gap-2 flex-wrap">
			{offered.has("telegram") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddTelegram.value = true;
					}}
				>
					<span className="icon icon-telegram" /> Connect Telegram
				</button>
			)}
			{offered.has("discord") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddDiscord.value = true;
					}}
				>
					<span className="icon icon-discord" /> Connect Discord
				</button>
			)}
			{offered.has("slack") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddSlack.value = true;
					}}
				>
					<span className="icon icon-slack" /> Connect Slack
				</button>
			)}
			{offered.has("matrix") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddMatrix.value = true;
					}}
				>
					<span className="icon icon-matrix" /> Connect Matrix
				</button>
			)}
			{offered.has("whatsapp") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddWhatsApp.value = true;
					}}
				>
					<span className="icon icon-whatsapp" /> Connect WhatsApp
				</button>
			)}
			{offered.has("signal") && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary inline-flex items-center gap-1.5"
					onClick={() => {
						if (connected.value) showAddSignal.value = true;
					}}
				>
					<span className="icon icon-signal" /> Connect Signal
				</button>
			)}
		</div>
	);
}

// ── Channels tab ─────────────────────────────────────────────

function ChannelsTab(): VNode {
	if (channels.value.length === 0) {
		return (
			<div className="text-center py-10">
				<div className="text-sm text-[var(--muted)] mb-4">No channels connected.</div>
				<div className="flex justify-center">
					<ConnectButtons />
				</div>
			</div>
		);
	}
	return (
		<>
			{channels.value.map((ch) => (
				<ChannelCard key={senderSelectionKey(ch)} channel={ch} />
			))}
		</>
	);
}

// ── Sender row renderer ──────────────────────────────────────

function renderSenderRow(s: SenderEntry, onAction: (identifier: string, action: string) => void): VNode {
	const identifier = s.username || s.peer_id;
	const lastSeenMs = s.last_seen ? s.last_seen * 1000 : 0;
	const usernameLabel = s.username ? (String(s.username).startsWith("@") ? s.username : `@${s.username}`) : "\u2014";
	const statusBadge = s.otp_pending ? (
		<button
			type="button"
			className="provider-item-badge cursor-pointer select-none border-0 font-[inherit]"
			style={{ background: "var(--warning-bg, #fef3c7)", color: "var(--warning-text, #92400e)" }}
			title="Copy OTP code"
			onClick={() => copyToClipboard(s.otp_pending?.code ?? "", "OTP code copied")}
		>
			OTP: <code className="text-xs">{s.otp_pending.code}</code>
		</button>
	) : (
		<span className={`provider-item-badge ${s.allowed ? "configured" : "oauth"}`}>
			{s.allowed ? "Allowed" : "Denied"}
		</span>
	);
	const actionBtn = s.allowed ? (
		<button
			type="button"
			className="provider-btn provider-btn-sm provider-btn-danger"
			onClick={() => onAction(identifier, "deny")}
		>
			Deny
		</button>
	) : (
		<button type="button" className="provider-btn provider-btn-sm" onClick={() => onAction(identifier, "approve")}>
			Approve
		</button>
	);
	return (
		<tr key={s.peer_id}>
			<td className="senders-td">{s.sender_name || s.peer_id}</td>
			<td className="senders-td" style={{ color: "var(--muted)" }}>
				{usernameLabel}
			</td>
			<td className="senders-td">{s.message_count}</td>
			<td className="senders-td" style={{ color: "var(--muted)", fontSize: "12px" }}>
				{lastSeenMs ? <time data-epoch-ms={String(lastSeenMs)}>{new Date(lastSeenMs).toISOString()}</time> : "\u2014"}
			</td>
			<td className="senders-td">{statusBadge}</td>
			<td className="senders-td">{actionBtn}</td>
		</tr>
	);
}

// ── Senders tab ──────────────────────────────────────────────

function SendersTab(): VNode {
	useEffect(() => {
		if (channels.value.length > 0 && !sendersAccount.value) {
			sendersAccount.value = senderSelectionKey(channels.value[0]);
		}
	}, [channels.value]);

	useEffect(() => {
		loadSenders();
	}, [sendersAccount.value]);

	if (channels.value.length === 0) {
		return <div className="text-sm text-[var(--muted)]">No channels configured.</div>;
	}

	function onAction(identifier: string, action: string): void {
		const rpc = action === "approve" ? "channels.senders.approve" : "channels.senders.deny";
		const parsed = parseSenderSelectionKey(sendersAccount.value);
		sendRpc(rpc, {
			type: parsed.type,
			account_id: parsed.account_id,
			identifier,
		}).then(() => {
			loadSenders();
			loadChannels();
		});
	}

	return (
		<div>
			<div style={{ marginBottom: "12px" }}>
				<label>
					<span className="text-xs text-[var(--muted)]" style={{ marginRight: "6px" }}>
						Account:
					</span>
					<select
						style={{
							background: "var(--surface2)",
							color: "var(--text)",
							border: "1px solid var(--border)",
							borderRadius: "4px",
							padding: "4px 8px",
							fontSize: "12px",
						}}
						value={sendersAccount.value}
						onChange={(e) => {
							sendersAccount.value = (e.target as HTMLSelectElement).value;
						}}
					>
						{channels.value.map((ch) => (
							<option key={senderSelectionKey(ch)} value={senderSelectionKey(ch)}>
								{ch.name || ch.account_id}
							</option>
						))}
					</select>
				</label>
			</div>
			{senders.value.length === 0 && (
				<div className="text-sm text-[var(--muted)] senders-empty">No messages received yet for this account.</div>
			)}
			{senders.value.length > 0 && (
				<table className="senders-table">
					<thead>
						<tr>
							<th className="senders-th">Sender</th>
							<th className="senders-th">Username</th>
							<th className="senders-th">Messages</th>
							<th className="senders-th">Last Seen</th>
							<th className="senders-th">Status</th>
							<th className="senders-th">Action</th>
						</tr>
					</thead>
					<tbody>{senders.value.map((s) => renderSenderRow(s, onAction))}</tbody>
				</table>
			)}
		</div>
	);
}

// ── Channel event handlers ───────────────────────────────────

function handleWhatsAppPairingEvent(p: ChannelEvent): void {
	if (p.kind === "pairing_qr_code" && p.account_id === waPairingAccountId.value) {
		waQrData.value = p.qr_data || null;
		waQrSvg.value = p.qr_svg || null;
	}
	if (p.kind === "pairing_complete" && p.account_id === waPairingAccountId.value) {
		showToast("WhatsApp connected!");
		showAddWhatsApp.value = false;
		waPairingAccountId.value = null;
		waQrData.value = null;
		waQrSvg.value = null;
		loadChannels();
	}
	if (p.kind === "pairing_failed" && p.account_id === waPairingAccountId.value) {
		waPairingError.value = p.reason || "Pairing failed";
	}
}

function handleChannelEvent(_payload: unknown): void {
	const p = _payload as ChannelEvent;
	if (p.kind === "otp_resolved") {
		loadChannels();
	}
	handleWhatsAppPairingEvent(p);
	if (p.kind === "pairing_complete" || p.kind === "account_disabled") {
		loadChannels();
	}
	const selected = parseSenderSelectionKey(sendersAccount.value || "");
	if (
		activeTab.value === "senders" &&
		selected.account_id === p.account_id &&
		selected.type === channelType(p.channel_type) &&
		(p.kind === "inbound_message" || p.kind === "otp_challenge" || p.kind === "otp_resolved")
	) {
		loadSenders();
	}
}

// ── Main page component ──────────────────────────────────────

function ChannelsPageComponent(): VNode {
	useEffect(() => {
		S.setRefreshChannelsPage(loadChannels);
		// Use prefetched cache for instant render
		if (S.cachedChannels !== null) channels.value = S.cachedChannels as Channel[];
		if (connected.value) loadChannels();

		const unsub = onEvent("channel", handleChannelEvent);
		S.setChannelEventUnsub(unsub);

		return () => {
			S.setRefreshChannelsPage(null);
			if (unsub) unsub();
			S.setChannelEventUnsub(null);
		};
	}, [connected.value]);

	const channelsTabs = computed(() => [
		{ id: "channels", label: "Channels", badge: channels.value.length || undefined },
		{ id: "senders", label: "Senders", badge: senders.value.length || undefined },
	]);

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<div className="flex items-center gap-3 flex-wrap">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Channels</h2>
				{activeTab.value === "channels" && channels.value.length > 0 && <ConnectButtons />}
			</div>
			<TabBar
				tabs={channelsTabs.value}
				active={activeTab.value}
				onChange={(id) => {
					activeTab.value = id;
				}}
			/>
			{activeTab.value === "channels" && <ChannelStorageNotice />}
			{activeTab.value === "channels" ? <ChannelsTab /> : <SendersTab />}
			<AddTelegramModal />
			<AddDiscordModal />
			<AddSlackModal />
			<AddMatrixModal />
			<AddSignalModal />
			<AddWhatsAppModal />
			<EditChannelModal />
			<ConfirmDialog />
		</div>
	);
}

// ── Mount / unmount exports ──────────────────────────────────

let _channelsContainer: HTMLElement | null = null;

export function initChannels(container: HTMLElement): void {
	_channelsContainer = container;
	container.style.cssText = "flex-direction:column;padding:0;overflow:hidden;";
	activeTab.value = "channels";
	showAddTelegram.value = false;
	showAddDiscord.value = false;
	showAddSlack.value = false;
	showAddMatrix.value = false;
	showAddSignal.value = false;
	showAddWhatsApp.value = false;
	editingChannel.value = null;
	sendersAccount.value = "";
	senders.value = [];
	render(<ChannelsPageComponent />, container);
}

export function teardownChannels(): void {
	S.setRefreshChannelsPage(null);
	if (S.channelEventUnsub) {
		S.channelEventUnsub();
		S.setChannelEventUnsub(null);
	}
	if (_channelsContainer) render(null, _channelsContainer);
	_channelsContainer = null;
}
