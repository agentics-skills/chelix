// ── Shared channel RPC wrappers and validation ────────────────
//

import { get as getGon } from "./gon";
import { sendRpc } from "./helpers";

export function channelStorageNote(): string {
	const dbPath = String(getGon("channel_storage_db_path") || "").trim();
	if (dbPath) {
		return `Channels added or edited in the web UI are stored in Chelix's internal database (${dbPath}). They are not written back to chelix.toml. The channel picker itself comes from [channels].offered in chelix.toml, so reload this page after editing that list.`;
	}
	return "Channels added or edited in the web UI are stored in Chelix's internal database (chelix.db). They are not written back to chelix.toml. The channel picker itself comes from [channels].offered in chelix.toml, so reload this page after editing that list.";
}

interface ValidationSuccess {
	valid: true;
}

interface ValidationFailure {
	valid: false;
	error: string;
}

type ValidationResult = ValidationSuccess | ValidationFailure;

/**
 * Validate required channel fields before submission.
 */
export function validateChannelFields(accountId: string, credential: string): ValidationResult {
	if (!accountId.trim()) {
		return { valid: false, error: "Account ID is required." };
	}
	if (!credential.trim()) {
		return {
			valid: false,
			error: "Bot token is required.",
		};
	}
	return { valid: true };
}

function randomSuffix(length: number): string {
	if (typeof window !== "undefined" && window.crypto?.getRandomValues) {
		const bytes = new Uint8Array(length);
		window.crypto.getRandomValues(bytes);
		return Array.from(bytes, (byte) => (byte % 36).toString(36)).join("");
	}
	let value = "";
	while (value.length < length) {
		value += Math.floor(Math.random() * 36).toString(36);
	}
	return value.slice(0, length);
}

/**
 * Generate a local Signal account identifier for Chelix.
 * Derive from the phone number when present, otherwise use a random suffix.
 */
export function deriveSignalAccountId(account?: string): string {
	const slug = String(account || "")
		.trim()
		.replace(/^\+/, "")
		.replace(/[^a-z0-9]+/gi, "-")
		.replace(/^-|-$/g, "");
	if (slug) return `signal-${slug}`.slice(0, 80);
	return `signal-${randomSuffix(6)}`;
}

interface ParseOk {
	ok: true;
	value: Record<string, unknown>;
}

interface ParseError {
	ok: false;
	error: string;
}

type ParseResult = ParseOk | ParseError;

/**
 * Parse an optional advanced channel config JSON object.
 */
export function parseChannelConfigPatch(text: string | null | undefined): ParseResult {
	const raw = String(text || "").trim();
	if (!raw) return { ok: true, value: {} };
	try {
		const value: unknown = JSON.parse(raw);
		if (!(value && typeof value === "object" && !Array.isArray(value))) {
			return { ok: false, error: "Advanced config must be a JSON object." };
		}
		return { ok: true, value: value as Record<string, unknown> };
	} catch (error) {
		const message = error instanceof Error ? error.message : String(error || "unknown error");
		return { ok: false, error: `Advanced config JSON is invalid: ${message}` };
	}
}

/**
 * Add a new channel (e.g. Telegram bot).
 */
export function addChannel(type: string, accountId: string, config: Record<string, unknown>): Promise<unknown> {
	return sendRpc("channels.add", { type, account_id: accountId, config });
}

/**
 * Fetch the current status of all configured channels.
 * Resolves with the RPC response; payload has `{ channels: [] }`.
 */
export function fetchChannelStatus(): Promise<unknown> {
	return sendRpc("channels.status", {});
}
