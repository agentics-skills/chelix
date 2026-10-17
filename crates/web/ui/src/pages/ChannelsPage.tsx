// ── Channels page (Preact + Signals) ──────────────────────────

import type { Signal } from "@preact/signals";
import { computed, signal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect } from "preact/hooks";
import { channelStorageNote, fetchChannelStatus } from "../channel-utils";
import { copyToClipboard } from "../clipboard";
import { TabBar } from "../components/forms/Tabs";
import { onEvent } from "../events";
import { get as getGon } from "../gon";
import { sendRpc } from "../helpers";
import { updateNavCount } from "../nav-counts";
import { connected } from "../signals";
import * as S from "../state";
import { showToast } from "../toast";
import { ConfirmDialog, requestConfirm } from "../ui";
import { AddSignalModal } from "./channels/modals/AddSignalModal";
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

interface ChannelExtra {
	qr_data?: string;
	qr_svg?: string;
}

export interface ChannelModelOverride {
	model: string;
	reasoning_effort: string;
}

/** Channel config fields (union of all channel types). */
export interface ChannelConfig {
	// Common
	token?: string;
	dm_policy?: string;
	group_policy?: string;
	mention_mode?: string;
	allowlist?: string[];
	model_override?: ChannelModelOverride;
	model_provider?: string;
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
export const showAddWhatsApp: Signal<boolean> = signal(false);
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
	if (t === "whatsapp") return "WhatsApp";
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
	webhook: "Webhook",
};

const MODE_HINTS: Record<string, string> = {
	webhook: "Requires a publicly reachable URL. Configure your platform to send events to the endpoint shown below.",
	polling: "Connects automatically via long-polling. No public URL needed.",
	gateway_loop: "Maintains a persistent connection. No public URL needed.",
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

function ChannelStorageNotice(): VNode {
	return (
		<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] px-3 py-2 text-xs text-[var(--muted)]">
			<span className="font-medium text-[var(--text-strong)]">Storage note.</span> {channelStorageNote()}
		</div>
	);
}

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
	if (t === "whatsapp") return <span className="icon icon-whatsapp" />;
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
	return {
		type,
		displayName: channel.name || channel.account_id || channelLabel(channel.type),
		statusClass: channel.status === "connected" ? "configured" : "oauth",
		sessionLine: channelSessionLine(channel.sessions),
		modeLabel: descriptor
			? MODE_LABELS[descriptor.capabilities.inbound_mode] || descriptor.capabilities.inbound_mode
			: null,
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
			<ChannelCardActions channel={ch} onRemove={onRemove} />
		</div>
	);
}

// ── Connect channel buttons ──────────────────────────────────

function ConnectButtons(): VNode {
	const offered = new Set((getGon("channels_offered") || ["telegram", "whatsapp"]) as string[]);
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
		// Use prefetched cache for instant render
		if (S.cachedChannels !== null) channels.value = S.cachedChannels as Channel[];
		if (connected.value) loadChannels();

		const unsub = onEvent("channel", handleChannelEvent);
		S.setChannelEventUnsub(unsub);

		return () => {
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
	showAddSignal.value = false;
	showAddWhatsApp.value = false;
	editingChannel.value = null;
	sendersAccount.value = "";
	senders.value = [];
	render(<ChannelsPageComponent />, container);
}

export function teardownChannels(): void {
	if (S.channelEventUnsub) {
		S.channelEventUnsub();
		S.setChannelEventUnsub(null);
	}
	if (_channelsContainer) render(null, _channelsContainer);
	_channelsContainer = null;
}
