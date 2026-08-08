// ── Channel types ───────────────────────────────────────────────
//
// Mirrors the Rust types in `crates/channels/src/plugin.rs`.

/**
 * Channel type identifier.
 * Serialised as lowercase via `#[serde(rename_all = "lowercase")]`.
 */
export type ChannelType = "telegram" | "whatsapp" | "discord" | "slack" | "matrix" | "signal" | "telephony";

/**
 * Runtime constants for `ChannelType` values.
 * Use `ChannelType.Telegram` etc. instead of bare string literals.
 */
export const ChannelType = {
	Telegram: "telegram" as const,
	WhatsApp: "whatsapp" as const,
	Discord: "discord" as const,
	Slack: "slack" as const,
	Matrix: "matrix" as const,
	Signal: "signal" as const,
	Telephony: "telephony" as const,
} satisfies Record<string, ChannelType>;

/**
 * How a channel receives inbound messages.
 * Serialised as snake_case via `#[serde(rename_all = "snake_case")]`.
 */
export type InboundMode = "none" | "polling" | "gateway_loop" | "socket_mode" | "webhook";

/** Static capability flags for a channel type. */
export interface ChannelCapabilities {
	inbound_mode: InboundMode;
	supports_outbound: boolean;
	supports_streaming: boolean;
	supports_interactive: boolean;
	supports_threads: boolean;
	supports_voice_ingest: boolean;
	supports_pairing: boolean;
	supports_otp: boolean;
	supports_reactions: boolean;
	supports_location: boolean;
}

/**
 * Full descriptor for a channel type, including capabilities.
 * Mirrors `ChannelDescriptor` in `crates/channels/src/plugin.rs`.
 * Injected in `gon.channel_descriptors`.
 */
export interface ChannelDescriptor {
	channel_type: ChannelType;
	display_name: string;
	capabilities: ChannelCapabilities;
}

/**
 * Where to send the LLM response back.
 * Mirrors `ChannelReplyTarget` in `crates/channels/src/plugin.rs`.
 * Stored as JSON in `SessionMeta.channelBinding`.
 */
export interface ChannelReplyTarget {
	channel_type: ChannelType;
	account_id: string;
	chat_id: string;
	message_id?: string;
	thread_id?: string;
}

/**
 * Client-side channel binding attached to a session.
 * A looser shape than `ChannelReplyTarget`, used by the session store
 * when the exact target fields are not needed.
 */
export interface ChannelBinding {
	type: string;
	account_id?: string;
	[key: string]: unknown;
}

/** Channel data as returned by the channels.status RPC. */
export interface ChannelInfo {
	type: string;
	account_id?: string;
	config?: Record<string, unknown>;
	enabled?: boolean;
	[key: string]: unknown;
}

/** Sender data as returned by the server. */
export interface SenderInfo {
	id?: string;
	name?: string;
	[key: string]: unknown;
}
