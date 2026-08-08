// ── Channel step (channel selection, complex forms) ───────────
//
// ChannelStep orchestrates channel type selection and delegates to
// per-channel form components. Simpler forms (Telegram, Discord)
// live in channel-forms.tsx; complex forms below.

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import {
	addChannel,
	deriveMatrixAccountId,
	MATRIX_DEFAULT_HOMESERVER,
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
	validateChannelFields,
} from "../../channel-utils";
import { onEvent } from "../../events";
import { get as getGon } from "../../gon";
import { sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { targetChecked, targetValue } from "../../typed-events";
import { WsEventName } from "../../types/ws-events";
import { ErrorPanel } from "../shared";
import type { ChannelFormProps } from "./channel-forms";
import {
	AdvancedConfigPatchField,
	ChannelStorageNotice,
	ChannelSuccess,
	ChannelTypeSelector,
	DiscordForm,
	SignalForm,
	TelegramForm,
} from "./channel-forms";

// ── Matrix form ─────────────────────────────────────────────

function MatrixForm({ onConnected, error, setError }: ChannelFormProps): VNode {
	const [homeserver, setHomeserver] = useState(MATRIX_DEFAULT_HOMESERVER);
	const [authMode, setAuthMode] = useState("password");
	const [userId, setUserId] = useState("");
	const [credential, setCredential] = useState("");
	const [deviceDisplayName, setDeviceDisplayName] = useState("");
	const [ownershipMode, setOwnershipMode] = useState("chelix_owned");
	const [dmPolicy, setDmPolicy] = useState("allowlist");
	const [roomPolicy, setRoomPolicy] = useState("allowlist");
	const [mentionMode, setMentionMode] = useState("mention");
	const [autoJoin, setAutoJoin] = useState("always");
	const [otpSelfApproval, setOtpSelfApproval] = useState(true);
	const [otpCooldown, setOtpCooldown] = useState("300");
	const [userAllowlist, setUserAllowlist] = useState("");
	const [roomAllowlist, setRoomAllowlist] = useState("");
	const [advancedConfig, setAdvancedConfig] = useState("");
	const [saving, setSaving] = useState(false);

	function splitLines(value: string): string[] {
		return value
			.trim()
			.split(/\n/)
			.map((s) => s.trim())
			.filter(Boolean);
	}

	function onSubmit(e: Event): void {
		e.preventDefault();
		const accountId = deriveMatrixAccountId({ userId, homeserver });
		const v = validateChannelFields("matrix", accountId, credential, {
			matrixAuthMode: authMode,
			matrixUserId: userId,
		});
		if (!v.valid) {
			setError(v.error);
			return;
		}
		if (!homeserver.trim()) {
			setError("Homeserver URL is required.");
			return;
		}
		const advancedPatch = parseChannelConfigPatch(advancedConfig);
		if (!advancedPatch.ok) {
			setError(advancedPatch.error);
			return;
		}
		setError(null);
		setSaving(true);
		const config: Record<string, unknown> = {
			homeserver: homeserver.trim(),
			ownership_mode:
				normalizeMatrixAuthMode(authMode) === "password" ? normalizeMatrixOwnershipMode(ownershipMode) : "user_managed",
			dm_policy: dmPolicy,
			room_policy: roomPolicy,
			mention_mode: mentionMode,
			auto_join: autoJoin,
			otp_self_approval: otpSelfApproval,
			otp_cooldown_secs: normalizeMatrixOtpCooldown(otpCooldown),
			user_allowlist: splitLines(userAllowlist),
			room_allowlist: splitLines(roomAllowlist),
		};
		if (normalizeMatrixAuthMode(authMode) === "password") {
			config.password = credential.trim();
		} else {
			config.access_token = credential.trim();
		}
		if (userId.trim()) config.user_id = userId.trim();
		if (deviceDisplayName.trim()) config.device_display_name = deviceDisplayName.trim();
		Object.assign(config, advancedPatch.value);
		(
			addChannel("matrix", accountId.trim(), config) as Promise<{
				ok?: boolean;
				error?: { message?: string; detail?: string };
			}>
		).then((res) => {
			setSaving(false);
			if (res?.ok) {
				onConnected(accountId.trim(), "matrix");
			} else {
				setError((res?.error && (res.error.message || res.error.detail)) || "Failed to connect Matrix.");
			}
		});
	}

	return (
		<form onSubmit={onSubmit} className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)] flex flex-col gap-1">
				<span className="font-medium text-[var(--text-strong)]">Connect a Matrix bot user</span>
				<span>
					1. Leave the homeserver as <span className="font-mono">{MATRIX_DEFAULT_HOMESERVER}</span> for matrix.org
					accounts
				</span>
				<span>
					2. Password is the default because it supports encrypted Matrix chats. Access token auth is only for plain
					Matrix traffic
				</span>
				<span>3. Chelix generates the local account ID automatically from the Matrix user or homeserver</span>
			</div>
			<div className="rounded-md border border-emerald-500/30 bg-emerald-500/10 p-3 text-xs text-emerald-100 flex flex-col gap-1">
				<span className="font-medium text-emerald-50">Encrypted chats require password auth</span>
				<span>{MATRIX_ENCRYPTION_GUIDANCE}</span>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Homeserver URL</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={homeserver}
						onInput={(e) => setHomeserver(targetValue(e))}
						placeholder={MATRIX_DEFAULT_HOMESERVER}
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="matrix_homeserver"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Authentication</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={authMode}
						onChange={(e) => setAuthMode(normalizeMatrixAuthMode(targetValue(e)))}
					>
						<option value="password">Password</option>
						<option value="access_token">Access token</option>
					</select>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">{matrixAuthModeGuidance(authMode)}</div>
			</div>
			{authMode === "password" ? (
				<label className="flex items-start gap-2 rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3">
					<input
						type="checkbox"
						aria-label="Let Chelix own this Matrix account"
						checked={normalizeMatrixOwnershipMode(ownershipMode) === "chelix_owned"}
						onChange={(e) => setOwnershipMode(targetChecked(e) ? "chelix_owned" : "user_managed")}
					/>
					<span className="flex flex-col gap-1">
						<span className="text-xs font-medium text-[var(--text-strong)]">Let Chelix own this Matrix account</span>
						<span className="text-xs text-[var(--muted)]">{matrixOwnershipModeGuidance(authMode, ownershipMode)}</span>
					</span>
				</label>
			) : (
				<div className="text-xs text-[var(--muted)]">{matrixOwnershipModeGuidance(authMode, "user_managed")}</div>
			)}
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">
						Matrix User ID{authMode === "password" ? " (required)" : " (optional)"}
					</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={userId}
						onInput={(e) => setUserId(targetValue(e))}
						placeholder="@bot:example.com"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="matrix_user_id"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">{matrixCredentialLabel(authMode)}</span>
					<input
						type="password"
						className="provider-key-input w-full"
						value={credential}
						onInput={(e) => setCredential(targetValue(e))}
						placeholder={matrixCredentialPlaceholder(authMode)}
						autoComplete="new-password"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="matrix_credential"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">
					{authMode === "password" ? (
						<>
							Use the password for the dedicated Matrix bot account. This is the required mode for encrypted Matrix
							chats because Chelix needs to create and persist its own Matrix device keys.
						</>
					) : (
						<>
							Get the access token in Element:{" "}
							<span className="font-mono">Settings -&gt; Help &amp; About -&gt; Advanced -&gt; Access Token</span>.
							Access token mode does <span className="font-medium">not</span> support encrypted Matrix chats because
							Chelix cannot import that existing device's private encryption keys.
						</>
					)}{" "}
					<a href={MATRIX_DOCS_URL} target="_blank" rel="noreferrer" className="text-[var(--accent)] underline">
						Matrix setup docs
					</a>
				</div>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Device Display Name (optional)</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={deviceDisplayName}
						onInput={(e) => setDeviceDisplayName(targetValue(e))}
						placeholder="Chelix Matrix Bot"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="matrix_device_display_name"
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
					<span className="text-xs text-[var(--muted)] mb-1 block">Room Policy</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={roomPolicy}
						onChange={(e) => setRoomPolicy(targetValue(e))}
					>
						<option value="allowlist">Room allowlist only (recommended)</option>
						<option value="open">Open (any joined room)</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Room Mention Mode</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={mentionMode}
						onChange={(e) => setMentionMode(targetValue(e))}
					>
						<option value="mention">Must mention bot</option>
						<option value="always">Always respond</option>
						<option value="none">Never respond in rooms</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Invite Auto-Join</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={autoJoin}
						onChange={(e) => setAutoJoin(targetValue(e))}
					>
						<option value="always">Always join invites</option>
						<option value="allowlist">Only when inviter or room is allowlisted</option>
						<option value="off">Do not auto-join</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Unknown DM Approval</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={otpSelfApproval ? "on" : "off"}
						onChange={(e) => setOtpSelfApproval(targetValue(e) !== "off")}
					>
						<option value="on">PIN challenge enabled (recommended)</option>
						<option value="off">Reject unknown DMs without a PIN</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">PIN Cooldown Seconds</span>
					<input
						type="number"
						min="1"
						step="1"
						className="provider-key-input w-full"
						value={otpCooldown}
						onInput={(e) => setOtpCooldown(targetValue(e))}
						name="matrix_otp_cooldown_secs"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">
					With DM policy on allowlist, unknown users get a 6-digit PIN challenge by default.
				</div>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">DM Allowlist (Matrix user IDs)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={userAllowlist}
						onInput={(e) => setUserAllowlist(targetValue(e))}
						placeholder="@alice:example.com"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Room Allowlist (room IDs or aliases)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={roomAllowlist}
						onInput={(e) => setRoomAllowlist(targetValue(e))}
						placeholder="!room:example.com"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
			</div>
			<AdvancedConfigPatchField value={advancedConfig} onInput={setAdvancedConfig} />
			{error && <ErrorPanel message={error} />}
			<button type="submit" className="provider-btn" disabled={saving}>
				{saving ? "Connecting\u2026" : "Connect Matrix"}
			</button>
		</form>
	);
}

// ── WhatsApp form ───────────────────────────────────────────

interface WhatsAppStatusChannel {
	type: string;
	account_id: string;
	status: string;
	extra?: { qr_data?: string; qr_svg?: string };
}

interface WhatsAppPairingPollContext {
	accountId: string;
	hadQr: boolean;
	onConnected: () => void;
	onQr: (data: string, svg?: string) => void;
}

async function pollWhatsAppPairing(context: WhatsAppPairingPollContext): Promise<void> {
	try {
		const response = (await sendRpc("channels.status", {})) as {
			ok?: boolean;
			payload?: { channels?: WhatsAppStatusChannel[] };
		};
		if (!response?.ok) return;
		const channel = (response.payload?.channels || []).find(
			(candidate) => candidate.type === "whatsapp" && candidate.account_id === context.accountId,
		);
		if (!channel) return;
		if (channel.status === "connected" || (context.hadQr && !channel.extra?.qr_data)) {
			context.onConnected();
			return;
		}
		if (channel.extra?.qr_data) context.onQr(channel.extra.qr_data, channel.extra.qr_svg);
	} catch {
		// Polling retries transient status errors on the next interval.
	}
}

function WhatsAppForm({ onConnected, error, setError }: ChannelFormProps): VNode {
	const [accountId, setAccountId] = useState("");
	const [dmPolicy, setDmPolicy] = useState("allowlist");
	const [allowlist, setAllowlist] = useState("");
	const [advancedConfig, setAdvancedConfig] = useState("");
	const [saving, setSaving] = useState(false);
	const [pairingStarted, setPairingStarted] = useState(false);
	const [qrData, setQrData] = useState<string | null>(null);
	const [qrSvg, setQrSvg] = useState<string | null>(null);
	const [qrSvgUrl, setQrSvgUrl] = useState<string | null>(null);
	const [pairingError, setPairingError] = useState<string | null>(null);
	const unsubRef = useRef<(() => void) | null>(null);
	const hadQrRef = useRef(false);

	useEffect(() => {
		return () => {
			if (unsubRef.current) unsubRef.current();
		};
	}, []);

	useEffect(() => {
		if (!pairingStarted) return undefined;
		const id = accountId.trim() || "main";
		const timer = setInterval(() => {
			void pollWhatsAppPairing({
				accountId: id,
				hadQr: hadQrRef.current,
				onConnected: () => onConnected(id, "whatsapp"),
				onQr: (data, svg) => {
					hadQrRef.current = true;
					setQrData(data);
					if (svg) setQrSvg(svg);
				},
			});
		}, 2000);
		return () => clearInterval(timer);
	}, [pairingStarted]);

	useEffect(() => {
		if (!qrSvg) {
			setQrSvgUrl(null);
			return undefined;
		}
		let nextUrl: string | null = null;
		try {
			nextUrl = URL.createObjectURL(new Blob([qrSvg], { type: "image/svg+xml" }));
			setQrSvgUrl(nextUrl);
		} catch (_err) {
			setQrSvgUrl(null);
		}
		return () => {
			if (nextUrl) URL.revokeObjectURL(nextUrl);
		};
	}, [qrSvg]);

	function onStartPairing(e: Event): void {
		e.preventDefault();
		const id = accountId.trim() || "main";
		const advancedPatch = parseChannelConfigPatch(advancedConfig);
		if (!advancedPatch.ok) {
			setError(advancedPatch.error);
			return;
		}
		setError(null);
		setSaving(true);
		setQrData(null);
		setQrSvg(null);
		setPairingError(null);
		if (unsubRef.current) unsubRef.current();
		unsubRef.current = onEvent(WsEventName.Channel, (p) => {
			if (p.account_id !== id) return;
			if (p.kind === "pairing_qr_code") {
				setQrData(p.qr_data as string);
				setQrSvg((p.qr_svg as string) || null);
			}
			if (p.kind === "pairing_complete") onConnected(id, "whatsapp");
			if (p.kind === "pairing_failed") setPairingError((p.reason as string) || "Pairing failed");
		});
		const allowlistEntries = allowlist
			.trim()
			.split(/\n/)
			.map((s) => s.trim())
			.filter(Boolean);
		const config: Record<string, unknown> = { dm_policy: dmPolicy, allowlist: allowlistEntries };
		Object.assign(config, advancedPatch.value);
		(
			addChannel("whatsapp", id, config) as Promise<{ ok?: boolean; error?: { message?: string; detail?: string } }>
		).then((res) => {
			setSaving(false);
			if (res?.ok) {
				setPairingStarted(true);
			} else {
				if (unsubRef.current) {
					unsubRef.current();
					unsubRef.current = null;
				}
				setError((res?.error && (res.error.message || res.error.detail)) || "Failed to start pairing.");
			}
		});
	}

	if (pairingStarted) {
		return (
			<div className="flex flex-col gap-4 items-center">
				{pairingError ? (
					<ErrorPanel message={pairingError} />
				) : qrData ? (
					<div
						className="rounded-lg bg-white p-3"
						style="width:200px;height:200px;display:flex;align-items:center;justify-content:center;"
					>
						{qrSvgUrl ? (
							<img src={qrSvgUrl} alt="WhatsApp pairing QR code" style="width:100%;height:100%;display:block;" />
						) : (
							<div className="text-center text-xs text-gray-600">
								<div style="font-family:monospace;font-size:9px;word-break:break-all;max-height:180px;overflow:hidden;">
									{qrData.substring(0, 200)}
								</div>
							</div>
						)}
					</div>
				) : (
					<div className="text-sm text-[var(--muted)]">Waiting for QR code...</div>
				)}
				<div className="text-xs text-[var(--muted)] text-center">
					Scan the QR code from your terminal, or open WhatsApp &gt; Settings &gt; Linked Devices &gt; Link a Device.
				</div>
				<div className="text-xs text-[var(--muted)] text-center italic">
					Only new messages will be processed. Past conversations are not synced.
				</div>
			</div>
		);
	}

	return (
		<form onSubmit={onStartPairing} className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)] flex flex-col gap-1">
				<span className="font-medium text-[var(--text-strong)]">Link your WhatsApp</span>
				<span>1. Click "Start Pairing" to generate a QR code</span>
				<span>2. Open WhatsApp &gt; Settings &gt; Linked Devices &gt; Link a Device</span>
				<span>3. Scan the QR code to connect</span>
				<span className="mt-1 italic">
					Only new messages will be processed &mdash; past conversations are not synced.
				</span>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Account ID (optional)</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={accountId}
						onInput={(e) => setAccountId(targetValue(e))}
						placeholder="main"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="whatsapp_account_id"
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
						<option value="open">Open (anyone)</option>
						<option value="allowlist">Allowlist only</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Allowlist (optional)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={allowlist}
						onInput={(e) => setAllowlist(targetValue(e))}
						placeholder="phone number or identifier"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">
					One per line. Only needed if DM policy is "Allowlist only".
				</div>
			</div>
			<AdvancedConfigPatchField value={advancedConfig} onInput={setAdvancedConfig} />
			{error && <ErrorPanel message={error} />}
			<button type="submit" className="provider-btn" disabled={saving}>
				{saving ? "Starting\u2026" : "Start Pairing"}
			</button>
		</form>
	);
}

// ── Slack form ──────────────────────────────────────────────

interface SlackCredentials {
	accountId: string;
	botToken: string;
	connectionMode: string;
	appToken: string;
	signingSecret: string;
}

function slackCredentialsError(credentials: SlackCredentials): string | null {
	if (!credentials.accountId.trim()) return "Account ID is required.";
	if (!credentials.botToken.trim()) return "Bot Token is required.";
	if (credentials.connectionMode === "socket_mode" && !credentials.appToken.trim()) {
		return "App Token is required for Socket Mode.";
	}
	if (credentials.connectionMode === "events_api" && !credentials.signingSecret.trim()) {
		return "Signing Secret is required for Events API mode.";
	}
	return null;
}

function buildSlackConfig(
	credentials: SlackCredentials,
	dmPolicy: string,
	allowlist: string,
	advancedPatch: Record<string, unknown>,
): Record<string, unknown> {
	const config: Record<string, unknown> = {
		bot_token: credentials.botToken.trim(),
		connection_mode: credentials.connectionMode,
		dm_policy: dmPolicy,
		mention_mode: "mention",
		allowlist: allowlist
			.trim()
			.split(/\n/)
			.map((value) => value.trim())
			.filter(Boolean),
	};
	if (credentials.connectionMode === "socket_mode") config.app_token = credentials.appToken.trim();
	if (credentials.connectionMode === "events_api") config.signing_secret = credentials.signingSecret.trim();
	return Object.assign(config, advancedPatch);
}

function SlackForm({ onConnected, error, setError }: ChannelFormProps): VNode {
	const [accountId, setAccountId] = useState("");
	const [botToken, setBotToken] = useState("");
	const [connectionMode, setConnectionMode] = useState("socket_mode");
	const [appToken, setAppToken] = useState("");
	const [signingSecret, setSigningSecret] = useState("");
	const [dmPolicy, setDmPolicy] = useState("allowlist");
	const [allowlist, setAllowlist] = useState("");
	const [advancedConfig, setAdvancedConfig] = useState("");
	const [saving, setSaving] = useState(false);

	function onSubmit(e: Event): void {
		e.preventDefault();
		const credentials = { accountId, botToken, connectionMode, appToken, signingSecret };
		const credentialsError = slackCredentialsError(credentials);
		if (credentialsError) {
			setError(credentialsError);
			return;
		}
		const advancedPatch = parseChannelConfigPatch(advancedConfig);
		if (!advancedPatch.ok) {
			setError(advancedPatch.error);
			return;
		}
		setError(null);
		setSaving(true);
		const config = buildSlackConfig(credentials, dmPolicy, allowlist, advancedPatch.value);
		(
			addChannel("slack", accountId.trim(), config) as Promise<{
				ok?: boolean;
				error?: { message?: string; detail?: string };
			}>
		).then((res) => {
			setSaving(false);
			if (res?.ok) {
				onConnected(accountId.trim(), "slack");
			} else {
				setError((res?.error && (res.error.message || res.error.detail)) || "Failed to connect Slack.");
			}
		});
	}

	return (
		<form onSubmit={onSubmit} className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)] flex flex-col gap-1">
				<span className="font-medium text-[var(--text-strong)]">How to set up a Slack bot</span>
				<span>
					1. Go to{" "}
					<a
						href="https://api.slack.com/apps"
						target="_blank"
						rel="noopener"
						className="text-[var(--accent)] underline"
					>
						api.slack.com/apps
					</a>{" "}
					and create a new app
				</span>
				<span>
					2. Under OAuth &amp; Permissions, add bot scopes: <code className="text-[var(--accent)]">chat:write</code>,{" "}
					<code className="text-[var(--accent)]">channels:history</code>,{" "}
					<code className="text-[var(--accent)]">im:history</code>,{" "}
					<code className="text-[var(--accent)]">app_mentions:read</code>
				</span>
				<span>3. Install the app to your workspace and copy the Bot User OAuth Token</span>
				<span>
					4. For Socket Mode: enable it and generate an App-Level Token with{" "}
					<code className="text-[var(--accent)]">connections:write</code> scope
				</span>
				<span>5. For Events API: set the Request URL to your server&rsquo;s webhook endpoint</span>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Account ID</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={accountId}
						onInput={(e) => setAccountId(targetValue(e))}
						placeholder="e.g. my-slack-bot"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="slack_account_id"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Bot Token (xoxb-...)</span>
					<input
						type="password"
						className="provider-key-input w-full"
						value={botToken}
						onInput={(e) => setBotToken(targetValue(e))}
						placeholder="xoxb-..."
						autoComplete="new-password"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="slack_bot_token"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Connection Mode</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={connectionMode}
						onChange={(e) => setConnectionMode(targetValue(e))}
					>
						<option value="socket_mode">Socket Mode (recommended)</option>
						<option value="events_api">Events API (HTTP webhook)</option>
					</select>
				</label>
			</div>
			{connectionMode === "socket_mode" && (
				<div>
					<label>
						<span className="text-xs text-[var(--muted)] mb-1 block">App Token (xapp-...)</span>
						<input
							type="password"
							className="provider-key-input w-full"
							value={appToken}
							onInput={(e) => setAppToken(targetValue(e))}
							placeholder="xapp-..."
							autoComplete="new-password"
							autoCapitalize="none"
							autoCorrect="off"
							spellcheck={false}
							name="slack_app_token"
						/>
					</label>
				</div>
			)}
			{connectionMode === "events_api" && (
				<div>
					<label>
						<span className="text-xs text-[var(--muted)] mb-1 block">Signing Secret</span>
						<input
							type="password"
							className="provider-key-input w-full"
							value={signingSecret}
							onInput={(e) => setSigningSecret(targetValue(e))}
							placeholder="Signing secret from Basic Information"
							autoComplete="new-password"
							autoCapitalize="none"
							autoCorrect="off"
							spellcheck={false}
							name="slack_signing_secret"
						/>
					</label>
				</div>
			)}
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
					<span className="text-xs text-[var(--muted)] mb-1 block">Allowed Slack user(s)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={allowlist}
						onInput={(e) => setAllowlist(targetValue(e))}
						placeholder="slack_username"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">One per line. These users can DM your bot.</div>
			</div>
			<AdvancedConfigPatchField value={advancedConfig} onInput={setAdvancedConfig} />
			{error && <ErrorPanel message={error} />}
			<button type="submit" className="provider-btn" disabled={saving}>
				{saving ? "Connecting\u2026" : "Connect Slack"}
			</button>
		</form>
	);
}

// ── ChannelStep ─────────────────────────────────────────────

export function ChannelStep({ onNext, onBack }: { onNext: () => void; onBack: () => void }): VNode {
	const offeredList = (getGon("channels_offered") as string[] | null) || [
		"telegram",
		"whatsapp",
		"discord",
		"slack",
		"matrix",
	];
	const offered = new Set(offeredList);
	const singleType = offeredList.length === 1 ? offeredList[0] : null;

	const [phase, setPhase] = useState<"select" | "form" | "success">(singleType ? "form" : "select");
	const [selectedType, setSelectedType] = useState<string | null>(singleType);
	const [connectedName, setConnectedName] = useState("");
	const [connectedType, setConnectedType] = useState<string | null>(null);
	const [channelError, setChannelError] = useState<string | null>(null);

	function onSelectType(type: string): void {
		setSelectedType(type);
		setPhase("form");
		setChannelError(null);
	}
	function onConnected(name: string, type: string): void {
		setConnectedName(name);
		setConnectedType(type);
		setPhase("success");
		setChannelError(null);
	}
	function onAnother(): void {
		if (singleType) {
			setPhase("form");
			setChannelError(null);
		} else {
			setPhase("select");
			setSelectedType(null);
			setChannelError(null);
		}
	}

	const showBackSelector = phase === "form" && !singleType;

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Connect a Channel</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Connect a messaging channel so you can chat from your phone or team workspace. You can set this up later in
				Channels.
			</p>
			<ChannelStorageNotice />
			{phase === "select" && <ChannelTypeSelector onSelect={onSelectType} offered={offered} />}
			{phase === "form" && selectedType === "telegram" && (
				<TelegramForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "whatsapp" && (
				<WhatsAppForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "discord" && (
				<DiscordForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "slack" && (
				<SlackForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "matrix" && (
				<MatrixForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "signal" && (
				<SignalForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "success" && connectedType && (
				<ChannelSuccess channelName={connectedName} channelType={connectedType} onAnother={onAnother} />
			)}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button
					type="button"
					className="provider-btn provider-btn-secondary"
					onClick={showBackSelector ? () => setPhase("select") : onBack}
				>
					{t("common:actions.back")}
				</button>
				{phase === "success" && (
					<button type="button" className="provider-btn" onClick={onNext}>
						{t("common:actions.continue")}
					</button>
				)}
				<button
					type="button"
					className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none underline"
					onClick={onNext}
				>
					{t("common:actions.skip")}
				</button>
			</div>
		</div>
	);
}
