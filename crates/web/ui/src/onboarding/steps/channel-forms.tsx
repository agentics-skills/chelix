// ── Channel form sub-components for onboarding ───────────────
//
// Shared helpers and the Telegram form.

import type { VNode } from "preact";
import { useState } from "preact/hooks";
import { addChannel, channelStorageNote, parseChannelConfigPatch, validateChannelFields } from "../../channel-utils";
import { targetValue } from "../../typed-events";
import { ErrorPanel } from "../shared";

// ── Types ───────────────────────────────────────────────────

export interface ChannelFormProps {
	onConnected: (name: string) => void;
	error: string | null;
	setError: (e: string | null) => void;
}

// ── Shared components ───────────────────────────────────────

export function ChannelStorageNotice(): VNode {
	return (
		<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)]">
			<span className="font-medium text-[var(--text-strong)]">Storage note.</span> {channelStorageNote()}
		</div>
	);
}

interface AdvancedConfigPatchFieldProps {
	value: string;
	onInput: (v: string) => void;
}

export function AdvancedConfigPatchField({ value, onInput }: AdvancedConfigPatchFieldProps): VNode {
	return (
		<details className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3">
			<summary className="cursor-pointer text-xs font-medium text-[var(--text-strong)]">Advanced Config JSON</summary>
			<div className="mt-3 flex flex-col gap-2">
				<div className="text-xs text-[var(--muted)]">
					Optional JSON object merged on top of the form before save. Use this for channel-specific settings that do not
					have dedicated fields yet.
				</div>
				<div>
					<label>
						<span className="text-xs text-[var(--muted)] mb-1 block">Advanced config JSON patch (optional)</span>
						<textarea
							name="channel_advanced_config"
							className="provider-key-input w-full min-h-[140px] font-mono text-xs"
							value={value}
							onInput={(e) => onInput(targetValue(e))}
							placeholder={'{"reply_to_message": true}'}
						/>
					</label>
				</div>
			</div>
		</details>
	);
}

// ── Channel type selector ───────────────────────────────────

interface ChannelTypeSelectorProps {
	onSelect: (type: string) => void;
	offered: Set<string>;
}

export function ChannelTypeSelector({ onSelect, offered }: ChannelTypeSelectorProps): VNode {
	const channelOptions: [string, string, string][] = (
		[["telegram", "icon-telegram", "Telegram"]] as [string, string, string][]
	).filter(([type]) => offered.has(type));

	return (
		<div className="grid grid-cols-2 gap-3 md:grid-cols-3">
			{channelOptions.map(([type, iconClass, label]) => (
				<button
					key={type}
					type="button"
					className="backend-card w-full min-h-[120px] items-center justify-center gap-4 px-4 py-8 text-center"
					onClick={() => onSelect(type)}
				>
					<span className={`icon icon-xl ${iconClass}`} />
					<span className="text-sm font-medium text-[var(--text-strong)]">{label}</span>
				</button>
			))}
		</div>
	);
}

// ── Channel success display ─────────────────────────────────

export function ChannelSuccess({ channelName, onAnother }: { channelName: string; onAnother: () => void }): VNode {
	return (
		<div className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--ok)] bg-[var(--surface)] p-4 flex gap-3 items-center">
				<span className="icon icon-lg icon-check-circle shrink-0" style="color:var(--ok)" />
				<div>
					<div className="text-sm font-medium text-[var(--text-strong)]">Channel connected</div>
					<div className="text-xs text-[var(--muted)] mt-0.5">
						{channelName} (Telegram) is now linked to your agent.
					</div>
				</div>
			</div>
			<button
				type="button"
				className="text-xs text-[var(--accent)] cursor-pointer bg-transparent border-none underline self-start"
				onClick={onAnother}
			>
				Connect another channel
			</button>
		</div>
	);
}

// ── Telegram form ───────────────────────────────────────────

export function TelegramForm({ onConnected, error, setError }: ChannelFormProps): VNode {
	const [accountId, setAccountId] = useState("");
	const [token, setToken] = useState("");
	const [dmPolicy, setDmPolicy] = useState("allowlist");
	const [allowlist, setAllowlist] = useState("");
	const [advancedConfig, setAdvancedConfig] = useState("");
	const [saving, setSaving] = useState(false);

	function onSubmit(e: Event): void {
		e.preventDefault();
		const v = validateChannelFields(accountId, token);
		if (!v.valid) {
			setError(v.error);
			return;
		}
		const advancedPatch = parseChannelConfigPatch(advancedConfig);
		if (!advancedPatch.ok) {
			setError(advancedPatch.error);
			return;
		}
		setError(null);
		setSaving(true);
		const allowlistEntries = allowlist
			.trim()
			.split(/\n/)
			.map((s) => s.trim())
			.filter(Boolean);
		const config: Record<string, unknown> = {
			token: token.trim(),
			dm_policy: dmPolicy,
			mention_mode: "mention",
			allowlist: allowlistEntries,
		};
		Object.assign(config, advancedPatch.value);
		(
			addChannel("telegram", accountId.trim(), config) as Promise<{
				ok?: boolean;
				error?: { message?: string; detail?: string };
			}>
		).then((res) => {
			setSaving(false);
			if (res?.ok) {
				onConnected(accountId.trim());
			} else {
				setError((res?.error && (res.error.message || res.error.detail)) || "Failed to connect bot.");
			}
		});
	}

	return (
		<form onSubmit={onSubmit} className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)] flex flex-col gap-1">
				<span className="font-medium text-[var(--text-strong)]">How to create a Telegram bot</span>
				<span>
					1. Open{" "}
					<a href="https://t.me/BotFather" target="_blank" rel="noopener" className="text-[var(--accent)] underline">
						@BotFather
					</a>{" "}
					in Telegram
				</span>
				<span>2. Send /newbot and follow the prompts</span>
				<span>3. Copy the bot token and paste it below</span>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Bot username</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={accountId}
						onInput={(e) => setAccountId(targetValue(e))}
						placeholder="e.g. my_assistant_bot"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="telegram_bot_username"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Bot token (from @BotFather)</span>
					<input
						type="password"
						className="provider-key-input w-full"
						value={token}
						onInput={(e) => setToken(targetValue(e))}
						placeholder="123456:ABC-DEF..."
						autoComplete="new-password"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="telegram_bot_token"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">DM Policy</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={dmPolicy}
						onChange={(e) => setDmPolicy(targetValue(e))}
					>
						<option value="allowlist">Allowlist only (recommended)</option>
						<option value="open">Open (anyone)</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Your Telegram username(s)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={allowlist}
						onInput={(e) => setAllowlist(targetValue(e))}
						placeholder="your_username"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">
					One username per line, without the @ sign. These users can DM your bot.
				</div>
			</div>
			<AdvancedConfigPatchField value={advancedConfig} onInput={setAdvancedConfig} />
			{error && <ErrorPanel message={error} />}
			<button type="submit" className="provider-btn" disabled={saving}>
				{saving ? "Connecting\u2026" : "Connect Bot"}
			</button>
		</form>
	);
}
