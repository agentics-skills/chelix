// ── Add Matrix modal ─────────────────────────────────────────

import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { useRef } from "preact/hooks";

import {
	addChannel,
	deriveMatrixAccountId,
	fetchChannelStatus,
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
} from "../../../channel-utils";
import { sendRpc } from "../../../helpers";
import { models as modelsSig } from "../../../stores/model-store";
import { targetChecked, targetValue } from "../../../typed-events";
import { ChannelType } from "../../../types/channel";
import { Modal, ModelSelect } from "../../../ui";
import { type ChannelConfig, ConnectionModeHint, loadChannels, showAddMatrix } from "../../ChannelsPage";
import { AdvancedConfigPatchField, AllowlistInput } from "../ChannelFields";

interface MatrixDraft {
	authMode: string;
	credential: string;
	homeserver: string;
	userId: string;
	accountId: string;
	deviceDisplayName: string;
	ownershipMode: string;
	modelId: string;
}

interface MatrixOidcStartResponse {
	ok?: boolean;
	payload?: { auth_url?: string };
	error?: { message?: string; detail?: string };
}

interface MatrixStatusResponse {
	ok?: boolean;
	payload?: { channels?: Array<{ account_id?: string; status?: string }> };
}

function formSelectValue(form: HTMLElement, field: string): string {
	return (form.querySelector(`[data-field=${field}]`) as HTMLSelectElement).value;
}

function selectedMatrixModelConfig(modelId: string): Pick<ChannelConfig, "model" | "model_provider"> {
	if (!modelId) return {};
	const provider = modelsSig.value.find((model) => model.id === modelId)?.provider;
	return provider ? { model: modelId, model_provider: provider } : { model: modelId };
}

function matrixBaseConfig(
	form: HTMLElement,
	draft: MatrixDraft,
	userAllowlist: string[],
	roomAllowlist: string[],
	otpSelfApproval: boolean,
	otpCooldown: string,
): ChannelConfig {
	return {
		homeserver: draft.homeserver,
		ownership_mode:
			draft.authMode === "access_token" ? "user_managed" : normalizeMatrixOwnershipMode(draft.ownershipMode),
		dm_policy: formSelectValue(form, "dmPolicy"),
		room_policy: formSelectValue(form, "roomPolicy"),
		mention_mode: formSelectValue(form, "mentionMode"),
		auto_join: formSelectValue(form, "autoJoin"),
		user_allowlist: userAllowlist,
		room_allowlist: roomAllowlist,
		otp_self_approval: otpSelfApproval,
		otp_cooldown_secs: normalizeMatrixOtpCooldown(otpCooldown),
		...selectedMatrixModelConfig(draft.modelId),
	};
}

function matrixDraftError(draft: MatrixDraft): string | null {
	const validation = validateChannelFields(ChannelType.Matrix, draft.accountId, draft.credential, {
		matrixAuthMode: draft.authMode,
		matrixUserId: draft.userId,
	});
	if (!validation.valid) return validation.error;
	return draft.homeserver ? null : "Homeserver URL is required.";
}

function matrixAddConfig(base: ChannelConfig, draft: MatrixDraft): ChannelConfig {
	const config = { ...base };
	if (draft.authMode === "password") config.password = draft.credential;
	else config.access_token = draft.credential;
	if (draft.userId) config.user_id = draft.userId;
	if (draft.deviceDisplayName) config.device_display_name = draft.deviceDisplayName;
	return config;
}

function defaultModelPlaceholder(): string {
	const defaultModel = modelsSig.value[0];
	return defaultModel ? `(default: ${defaultModel.display_name || defaultModel.id})` : "(server default)";
}

export function AddMatrixModal(): VNode {
	const error = useSignal("");
	const saving = useSignal(false);
	const addModel = useSignal("");
	const userAllowlistItems = useSignal<string[]>([]);
	const roomAllowlistItems = useSignal<string[]>([]);
	const homeserverDraft = useSignal(MATRIX_DEFAULT_HOMESERVER);
	const authModeDraft = useSignal("oidc");
	const userIdDraft = useSignal("");
	const credentialDraft = useSignal("");
	const deviceDisplayNameDraft = useSignal("");
	const ownershipModeDraft = useSignal("chelix_owned");
	const oidcWaiting = useSignal(false);
	const oidcPollRef = useRef<ReturnType<typeof setInterval> | null>(null);
	const otpSelfApprovalDraft = useSignal(true);
	const otpCooldownDraft = useSignal("300");
	const advancedConfigPatch = useSignal("");

	function resetForm(): void {
		if (oidcPollRef.current) {
			clearInterval(oidcPollRef.current);
			oidcPollRef.current = null;
		}
		addModel.value = "";
		userAllowlistItems.value = [];
		roomAllowlistItems.value = [];
		homeserverDraft.value = MATRIX_DEFAULT_HOMESERVER;
		authModeDraft.value = "oidc";
		userIdDraft.value = "";
		credentialDraft.value = "";
		deviceDisplayNameDraft.value = "";
		ownershipModeDraft.value = "chelix_owned";
		otpSelfApprovalDraft.value = true;
		otpCooldownDraft.value = "300";
		advancedConfigPatch.value = "";
		oidcWaiting.value = false;
	}

	function finishOidcConnection(poll: ReturnType<typeof setInterval>): void {
		clearInterval(poll);
		oidcPollRef.current = null;
		oidcWaiting.value = false;
		showAddMatrix.value = false;
		resetForm();
		loadChannels();
	}

	function handleOidcStatus(statusRes: unknown, poll: ReturnType<typeof setInterval>, accountId: string): void {
		if (oidcPollRef.current !== poll) return;
		const response = statusRes as MatrixStatusResponse;
		if (!response?.ok) return;
		const connected = (response.payload?.channels || []).some(
			(channel) => channel.account_id === accountId && channel.status === "connected",
		);
		if (connected) finishOidcConnection(poll);
	}

	function startOidcPolling(accountId: string): void {
		let pollCount = 0;
		const poll = setInterval(() => {
			pollCount++;
			if (pollCount > 120) {
				clearInterval(poll);
				if (oidcPollRef.current === poll) oidcPollRef.current = null;
				oidcWaiting.value = false;
				error.value = "OIDC authentication timed out. Please try again.";
				return;
			}
			fetchChannelStatus().then((statusRes: unknown) => {
				handleOidcStatus(statusRes, poll, accountId);
			});
		}, 1000);
		oidcPollRef.current = poll;
	}

	function handleOidcStart(responseValue: unknown, accountId: string): void {
		const response = responseValue as MatrixOidcStartResponse;
		saving.value = false;
		if (!(response?.ok && response.payload?.auth_url)) {
			error.value = response?.error?.message || response?.error?.detail || "Failed to start OIDC login.";
			return;
		}
		oidcWaiting.value = true;
		window.open(response.payload.auth_url, "_blank", "noopener");
		startOidcPolling(accountId);
	}

	function handleMatrixAdd(responseValue: unknown): void {
		saving.value = false;
		const response = responseValue as MatrixOidcStartResponse;
		if (!response?.ok) {
			error.value = response?.error?.message || response?.error?.detail || "Failed to connect Matrix.";
			return;
		}
		showAddMatrix.value = false;
		resetForm();
		loadChannels();
	}

	function onSubmit(e: Event): void {
		e.preventDefault();
		const form = (e.target as HTMLElement).closest(".channel-form") as HTMLElement;
		const homeserver = homeserverDraft.value.trim();
		const userId = userIdDraft.value.trim();
		const draft: MatrixDraft = {
			authMode: normalizeMatrixAuthMode(authModeDraft.value),
			credential: credentialDraft.value.trim(),
			homeserver,
			userId,
			accountId: deriveMatrixAccountId({ userId, homeserver }),
			deviceDisplayName: deviceDisplayNameDraft.value.trim(),
			ownershipMode: ownershipModeDraft.value,
			modelId: addModel.value,
		};
		const validationError = matrixDraftError(draft);
		if (validationError) {
			error.value = validationError;
			return;
		}
		const advancedPatch = parseChannelConfigPatch(advancedConfigPatch.value);
		if (!advancedPatch.ok) {
			error.value = advancedPatch.error;
			return;
		}
		error.value = "";
		saving.value = true;

		const baseConfig = matrixBaseConfig(
			form,
			draft,
			userAllowlistItems.value,
			roomAllowlistItems.value,
			otpSelfApprovalDraft.value,
			otpCooldownDraft.value,
		);
		if (draft.deviceDisplayName) baseConfig.device_display_name = draft.deviceDisplayName;
		Object.assign(baseConfig, advancedPatch.value);
		if (draft.authMode === "oidc") {
			sendRpc("channels.oauth_start", {
				account_id: draft.accountId,
				homeserver: draft.homeserver,
				redirect_uri: `${window.location.origin}/auth/callback`,
				config: baseConfig,
			}).then((response) => {
				handleOidcStart(response, draft.accountId);
			});
			return;
		}
		const addConfig = matrixAddConfig(baseConfig, draft);
		addChannel(ChannelType.Matrix, draft.accountId, addConfig).then(handleMatrixAdd);
	}

	const defaultPlaceholder = defaultModelPlaceholder();

	return (
		<Modal
			show={showAddMatrix.value}
			onClose={() => {
				showAddMatrix.value = false;
			}}
			title="Connect Matrix"
		>
			<div className="channel-form">
				<div className="channel-card">
					<div>
						<span className="text-xs font-medium text-[var(--text-strong)]">Connect a Matrix bot user</span>
						<div className="text-xs text-[var(--muted)] channel-help">
							1. Leave the homeserver as <span className="font-mono">{MATRIX_DEFAULT_HOMESERVER}</span> for matrix.org
							accounts
						</div>
						<div className="text-xs text-[var(--muted)]">
							2. OIDC is the default because it is the simplest and supports encrypted Matrix chats. Password also
							supports encryption. Access token auth is only for plain Matrix traffic
						</div>
						<div className="text-xs text-[var(--muted)]">
							3. Chelix generates the local account ID automatically from the Matrix user or homeserver
						</div>
					</div>
				</div>
				<div className="rounded-md border border-emerald-600/30 bg-emerald-50 px-3 py-2 text-xs text-emerald-900">
					<div className="font-medium text-emerald-800">Encrypted chats require OIDC or Password auth</div>
					<div>{MATRIX_ENCRYPTION_GUIDANCE}</div>
				</div>
				<ConnectionModeHint type={ChannelType.Matrix} />
				<label>
					<span className="text-xs text-[var(--muted)]">Homeserver URL</span>
					<input
						data-field="homeserver"
						type="text"
						placeholder={MATRIX_DEFAULT_HOMESERVER}
						value={homeserverDraft.value}
						onInput={(e) => {
							homeserverDraft.value = targetValue(e);
						}}
						className="channel-input"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
					/>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Authentication</span>
					<select
						data-field="authMode"
						className="channel-select"
						value={authModeDraft.value}
						onChange={(e) => {
							authModeDraft.value = normalizeMatrixAuthMode(targetValue(e));
						}}
					>
						<option value="oidc">OIDC (recommended)</option>
						<option value="password">Password</option>
						<option value="access_token">Access token</option>
					</select>
				</label>
				<div className="text-xs text-[var(--muted)]">{matrixAuthModeGuidance(authModeDraft.value)}</div>
				{authModeDraft.value === "password" || authModeDraft.value === "oidc" ? (
					<label className="flex items-start gap-2 rounded-md border border-[var(--border)] bg-[var(--surface2)] px-3 py-2">
						<input
							type="checkbox"
							aria-label="Let Chelix own this Matrix account"
							checked={normalizeMatrixOwnershipMode(ownershipModeDraft.value) === "chelix_owned"}
							onChange={(e) => {
								ownershipModeDraft.value = targetChecked(e) ? "chelix_owned" : "user_managed";
							}}
						/>
						<span className="flex flex-col gap-1">
							<span className="text-xs font-medium text-[var(--text-strong)]">Let Chelix own this Matrix account</span>
							<span className="text-xs text-[var(--muted)]">
								{matrixOwnershipModeGuidance(authModeDraft.value, ownershipModeDraft.value)}
							</span>
						</span>
					</label>
				) : (
					<div className="text-xs text-[var(--muted)]">
						{matrixOwnershipModeGuidance(authModeDraft.value, "user_managed")}
					</div>
				)}
				{authModeDraft.value !== "oidc" && (
					<>
						<label>
							<span className="text-xs text-[var(--muted)]">
								Matrix User ID{authModeDraft.value === "password" ? " (required)" : " (optional)"}
							</span>
							<input
								data-field="userId"
								type="text"
								placeholder="@bot:example.com"
								value={userIdDraft.value}
								onInput={(e) => {
									userIdDraft.value = targetValue(e);
								}}
								className="channel-input"
							/>
						</label>
						<label>
							<span className="text-xs text-[var(--muted)]">{matrixCredentialLabel(authModeDraft.value)}</span>
							<input
								data-field="credential"
								type="password"
								placeholder={matrixCredentialPlaceholder(authModeDraft.value)}
								value={credentialDraft.value}
								onInput={(e) => {
									credentialDraft.value = targetValue(e);
								}}
								className="channel-input"
								autoComplete="new-password"
								autoCapitalize="none"
								autoCorrect="off"
								spellcheck={false}
							/>
						</label>
						<div className="text-xs text-[var(--muted)]">
							{authModeDraft.value === "password" ? (
								"Use the password for the dedicated Matrix bot account. This is the required mode for encrypted Matrix chats because Chelix needs to create and persist its own Matrix device keys."
							) : (
								<>
									Get the access token in Element:{" "}
									<span className="font-mono">Settings -&gt; Help & About -&gt; Advanced -&gt; Access Token</span>.
									Access token mode does <span className="font-medium">not</span> support encrypted Matrix chats because
									Chelix cannot import that existing device's private encryption keys.
								</>
							)}{" "}
							<a href={MATRIX_DOCS_URL} target="_blank" rel="noreferrer" className="text-[var(--accent)] underline">
								Matrix setup docs
							</a>
						</div>
					</>
				)}
				<label>
					<span className="text-xs text-[var(--muted)]">Device Display Name (optional)</span>
					<input
						data-field="deviceDisplayName"
						type="text"
						placeholder="Chelix Matrix Bot"
						value={deviceDisplayNameDraft.value}
						onInput={(e) => {
							deviceDisplayNameDraft.value = targetValue(e);
						}}
						className="channel-input"
					/>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">DM Policy</span>
					<select data-field="dmPolicy" className="channel-select">
						<option value="allowlist">Allowlist only</option>
						<option value="open">Open (anyone)</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Room Policy</span>
					<select data-field="roomPolicy" className="channel-select">
						<option value="allowlist">Room allowlist only</option>
						<option value="open">Open (any joined room)</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Room Mention Mode</span>
					<select data-field="mentionMode" className="channel-select">
						<option value="mention">Must mention bot</option>
						<option value="always">Always respond</option>
						<option value="none">Never respond in rooms</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Invite Auto-Join</span>
					<select data-field="autoJoin" className="channel-select">
						<option value="always">Always join invites</option>
						<option value="allowlist">Only when inviter or room is allowlisted</option>
						<option value="off">Do not auto-join</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Unknown DM Approval</span>
					<select
						data-field="otpSelfApproval"
						className="channel-select"
						value={otpSelfApprovalDraft.value ? "on" : "off"}
						onChange={(e) => {
							otpSelfApprovalDraft.value = targetValue(e) !== "off";
						}}
					>
						<option value="on">PIN challenge enabled (recommended)</option>
						<option value="off">Reject unknown DMs without a PIN</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">PIN Cooldown Seconds</span>
					<input
						data-field="otpCooldown"
						type="number"
						min={1}
						step={1}
						className="channel-input"
						value={otpCooldownDraft.value}
						onInput={(e) => {
							otpCooldownDraft.value = targetValue(e);
						}}
					/>
				</label>
				<div className="text-xs text-[var(--muted)]">
					With DM policy on allowlist, unknown users get a 6-digit PIN challenge by default.
				</div>
				<span className="text-xs text-[var(--muted)]">Default Model</span>
				<ModelSelect
					ariaLabel="Default Model"
					models={modelsSig.value}
					value={addModel.value}
					onChange={(v: string) => {
						addModel.value = v;
					}}
					placeholder={defaultPlaceholder}
				/>
				<span className="text-xs text-[var(--muted)]">DM Allowlist (Matrix user IDs)</span>
				<AllowlistInput
					ariaLabel="DM Allowlist (Matrix user IDs)"
					value={userAllowlistItems.value}
					preserveAt={true}
					onChange={(items) => {
						userAllowlistItems.value = items;
					}}
				/>
				<span className="text-xs text-[var(--muted)]">Room Allowlist (room IDs or aliases)</span>
				<AllowlistInput
					ariaLabel="Room Allowlist (room IDs or aliases)"
					value={roomAllowlistItems.value}
					preserveAt={true}
					onChange={(items) => {
						roomAllowlistItems.value = items;
					}}
				/>
				<AdvancedConfigPatchField
					value={advancedConfigPatch.value}
					onInput={(value) => {
						advancedConfigPatch.value = value;
					}}
				/>
				{error.value && <div className="text-xs text-[var(--error)] py-1">{error.value}</div>}
				<button type="button" className="provider-btn" onClick={onSubmit} disabled={saving.value || oidcWaiting.value}>
					{saving.value
						? "Connecting\u2026"
						: oidcWaiting.value
							? "Waiting for OIDC\u2026"
							: authModeDraft.value === "oidc"
								? "Authenticate with OIDC"
								: "Connect Matrix"}
				</button>
			</div>
		</Modal>
	);
}
