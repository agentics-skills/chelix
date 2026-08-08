// ── Auth step (passkey + password setup) ─────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { t } from "../../i18n";
import { detectPasskeyName } from "../../passkey-detect";
import { targetValue } from "../../typed-events";
import { copyToClipboard } from "../../ui";
import { prepareCreationOptions } from "../../webauthn-helpers";
import { bufferToBase64, ErrorPanel, ensureWsConnected } from "../shared";

type AuthMethod = "passkey" | "password" | null;

interface AuthStatusPayload {
	setup_code_required?: boolean;
	localhost_only?: boolean;
	webauthn_available?: boolean;
	passkey_origins?: string[];
	setup_complete?: boolean;
}

type AuthSetupResult = { ok: true; recoveryKey: string | null } | { ok: false; error: string };
type AuthActionResult = { ok: true } | { ok: false; error: string };

function setupCodeError(required: boolean, code: string): string | null {
	return required && code.trim().length === 0 ? "Enter the setup code shown in the process log (stdout)." : null;
}

function passwordSetupError(
	password: string,
	confirmation: string,
	localhostOnly: boolean,
	codeRequired: boolean,
	setupCode: string,
): string | null {
	if (password.length > 0 || !localhostOnly) {
		if (password.length < 12) return "Password must be at least 12 characters.";
		if (password !== confirmation) return "Passwords do not match.";
	}
	return setupCodeError(codeRequired, setupCode);
}

async function submitPasswordSetup(
	password: string,
	setupCode: string,
	codeRequired: boolean,
): Promise<AuthSetupResult> {
	const body: Record<string, string> = password ? { password } : {};
	if (codeRequired) body.setup_code = setupCode.trim();
	try {
		const response = await fetch("/api/auth/setup", {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(body),
		});
		if (!response.ok) return { ok: false, error: (await response.text()) || "Setup failed" };
		try {
			const payload = (await response.json()) as { recovery_key?: string };
			return { ok: true, recoveryKey: payload.recovery_key || null };
		} catch {
			return { ok: true, recoveryKey: null };
		}
	} catch (error) {
		return { ok: false, error: (error as Error).message };
	}
}

function optionalPasswordError(password: string, confirmation: string): string | null {
	if (password.length < 12) return "Password must be at least 12 characters.";
	return password === confirmation ? null : "Passwords do not match.";
}

async function submitOptionalPassword(password: string): Promise<AuthActionResult> {
	try {
		const response = await fetch("/api/auth/password/change", {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ new_password: password }),
		});
		if (response.ok) return { ok: true };
		return { ok: false, error: (await response.text()) || "Failed to set password" };
	} catch (error) {
		return { ok: false, error: (error as Error).message };
	}
}

interface PasskeyBeginPayload {
	options: Record<string, unknown>;
	challenge_id: string;
}

interface CreatedPasskey {
	credential: PublicKeyCredential;
	challengeId: string;
	rpId: string | null;
}

async function beginPasskeyRegistration(setupCode: string | null): Promise<PasskeyBeginPayload> {
	const response = await fetch("/api/auth/setup/passkey/register/begin", {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify(setupCode ? { setup_code: setupCode } : {}),
	});
	if (!response.ok) throw new Error((await response.text()) || "Failed to start passkey registration");
	return (await response.json()) as PasskeyBeginPayload;
}

function passkeyRpId(payload: PasskeyBeginPayload): string | null {
	const options = payload.options.publicKey as Record<string, unknown>;
	return (options.rp as Record<string, string>)?.id || null;
}

async function createPasskey(payload: PasskeyBeginPayload, rpId: string | null): Promise<CreatedPasskey> {
	const options = payload.options.publicKey as Record<string, unknown>;
	const credential = (await navigator.credentials.create({
		publicKey: prepareCreationOptions(options),
	})) as PublicKeyCredential | null;
	if (!credential) throw new Error("Passkey registration was cancelled.");
	return { credential, challengeId: payload.challenge_id, rpId };
}

function passkeyFinishBody(created: CreatedPasskey, name: string, setupCode: string | null): Record<string, unknown> {
	const attestation = created.credential.response as AuthenticatorAttestationResponse;
	const body: Record<string, unknown> = {
		challenge_id: created.challengeId,
		name: name.trim() || detectPasskeyName(created.credential),
		credential: {
			id: created.credential.id,
			rawId: bufferToBase64(created.credential.rawId),
			type: created.credential.type,
			response: {
				attestationObject: bufferToBase64(attestation.attestationObject),
				clientDataJSON: bufferToBase64(attestation.clientDataJSON),
			},
		},
	};
	if (setupCode) body.setup_code = setupCode;
	return body;
}

async function finishPasskeyRegistration(
	created: CreatedPasskey,
	name: string,
	setupCode: string | null,
): Promise<void> {
	const response = await fetch("/api/auth/setup/passkey/register/finish", {
		method: "POST",
		headers: { "Content-Type": "application/json" },
		body: JSON.stringify(passkeyFinishBody(created, name, setupCode)),
	});
	if (!response.ok) throw new Error((await response.text()) || "Passkey registration failed");
}

function passkeyRegistrationError(error: Error & { name?: string }, rpId: string | null): string {
	if (error.name === "NotAllowedError") return "Passkey registration was cancelled.";
	const message = error.message || "Passkey registration failed";
	return rpId ? `${message} (RPID: "${rpId}", current origin: "${location.origin}")` : message;
}

async function registerPasskey(name: string, setupCode: string | null): Promise<AuthActionResult> {
	let rpId: string | null = null;
	try {
		const payload = await beginPasskeyRegistration(setupCode);
		rpId = passkeyRpId(payload);
		const created = await createPasskey(payload, rpId);
		await finishPasskeyRegistration(created, name, setupCode);
		return { ok: true };
	} catch (error) {
		return { ok: false, error: passkeyRegistrationError(error as Error & { name?: string }, rpId) };
	}
}

function passkeyDisabledReason(available: boolean, browserSupported: boolean, ipAddress: boolean): string | null {
	if (!available) return "Not available on this server";
	if (!browserSupported) return "Browser not supported";
	return ipAddress ? "Requires domain name" : null;
}

function AuthConfiguredView({ onNext }: { onNext: () => void }): VNode {
	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:auth.secureYourInstance")}</h2>
			<div className="flex items-center gap-2 text-sm text-[var(--accent)]">
				<span className="icon icon-checkmark" />
				Authentication is already configured.
			</div>
			<button type="button" className="provider-btn self-start" onClick={onNext}>
				Next
			</button>
		</div>
	);
}

function RecoveryKeyView({
	recoveryKey,
	copied,
	onCopy,
	onNext,
}: {
	recoveryKey: string;
	copied: boolean;
	onCopy: () => void;
	onNext: () => void;
}): VNode {
	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Secure your instance</h2>
			<div className="flex items-center gap-2 text-sm text-[var(--accent)]">
				<span className="icon icon-checkmark" />
				Password set and vault initialized
			</div>
			<div
				style={{
					maxWidth: "600px",
					padding: "12px 16px",
					borderRadius: "6px",
					border: "1px solid var(--border)",
					background: "var(--bg)",
				}}
			>
				<div className="text-xs text-[var(--muted)]" style={{ marginBottom: "8px" }}>
					Recovery key
				</div>
				<code
					className="select-all break-all"
					style={{
						fontFamily: "var(--font-mono)",
						fontSize: ".8rem",
						color: "var(--text-strong)",
						display: "block",
						lineHeight: "1.5",
					}}
				>
					{recoveryKey}
				</code>
				<button type="button" className="provider-btn provider-btn-secondary mt-2.5" onClick={onCopy}>
					{copied ? "Copied!" : "Copy"}
				</button>
			</div>
			<div className="text-xs" style={{ color: "var(--error)", maxWidth: "600px" }}>
				Save this recovery key in a safe place. It will not be shown again. You need it to unlock the vault if you
				forget your password.
			</div>
			<button type="button" className="provider-btn self-start" onClick={onNext}>
				Continue
			</button>
		</div>
	);
}

interface OptionalPasswordViewProps {
	password: string;
	confirmation: string;
	setPassword: (value: string) => void;
	setConfirmation: (value: string) => void;
	saving: boolean;
	error: string | null;
	onSubmit: (event: Event) => void;
	onSkip: () => void;
}

function OptionalPasswordView(props: OptionalPasswordViewProps): VNode {
	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:auth.secureYourInstance")}</h2>
			<div className="flex items-center gap-2 text-sm text-[var(--accent)]">
				<span className="icon icon-checkmark" />
				Passkey registered successfully!
			</div>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Optionally set a password as a fallback for when passkeys aren't available.
			</p>
			<form onSubmit={props.onSubmit} className="flex flex-col gap-3">
				<label htmlFor="onboarding-passkey-password" className="text-xs text-[var(--muted)] mb-1 block">
					Password
					<input
						id="onboarding-passkey-password"
						type="password"
						name="password"
						autoComplete="new-password"
						className="provider-key-input w-full"
						value={props.password}
						onInput={(event) => props.setPassword(targetValue(event))}
						placeholder="At least 12 characters"
						autofocus
					/>
				</label>
				<label htmlFor="onboarding-passkey-password-confirm" className="text-xs text-[var(--muted)] mb-1 block">
					Confirm password
					<input
						id="onboarding-passkey-password-confirm"
						type="password"
						name="confirm_password"
						autoComplete="new-password"
						className="provider-key-input w-full"
						value={props.confirmation}
						onInput={(event) => props.setConfirmation(targetValue(event))}
						placeholder="Repeat password"
					/>
				</label>
				{props.error && <ErrorPanel message={props.error} />}
				<div className="flex flex-wrap items-center gap-3 mt-1">
					<button type="submit" className="provider-btn" disabled={props.saving}>
						{props.saving ? "Setting\u2026" : "Set password & continue"}
					</button>
					<button
						type="button"
						className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none underline"
						onClick={props.onSkip}
					>
						Skip
					</button>
				</div>
			</form>
		</div>
	);
}

function SetupCodeField({ value, onInput }: { value: string; onInput: (value: string) => void }): VNode {
	return (
		<div>
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">Setup code</span>
				<input
					type="text"
					className="provider-key-input w-full"
					inputMode="numeric"
					pattern="[0-9]*"
					value={value}
					onInput={(event) => onInput(targetValue(event))}
					placeholder="6-digit code from terminal"
				/>
			</label>
			<div className="text-xs text-[var(--muted)] mt-1">Find this code in the chelix process log (stdout).</div>
		</div>
	);
}

interface AuthMethodViewProps {
	method: AuthMethod;
	setMethod: (method: AuthMethod) => void;
	passkeyEnabled: boolean;
	passkeyDisabledReason: string | null;
	passkeyName: string;
	setPasskeyName: (value: string) => void;
	originsHint: string | null;
	password: string;
	setPassword: (value: string) => void;
	confirmation: string;
	setConfirmation: (value: string) => void;
	localhostOnly: boolean;
	saving: boolean;
	error: string | null;
	skippable: boolean;
	onPasskey: () => void;
	onPassword: (event: Event) => void;
	onSkip: () => void;
}

function SkipAuthButton({ onClick }: { onClick: () => void }): VNode {
	return (
		<button
			type="button"
			className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none underline"
			onClick={onClick}
		>
			{t("common:actions.skip")}
		</button>
	);
}

function PasskeyMethodForm(props: AuthMethodViewProps): VNode {
	return (
		<div className="flex flex-col gap-3">
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">Passkey name</span>
				<input
					type="text"
					className="provider-key-input w-full"
					value={props.passkeyName}
					onInput={(event) => props.setPasskeyName(targetValue(event))}
					placeholder="e.g. MacBook Touch ID (optional)"
				/>
			</label>
			{props.originsHint && (
				<div className="text-xs text-[var(--muted)]">Passkeys will work when visiting: {props.originsHint}</div>
			)}
			{props.error && <ErrorPanel message={props.error} />}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button type="button" className="provider-btn" disabled={props.saving} onClick={props.onPasskey}>
					{props.saving ? "Registering\u2026" : "Register passkey"}
				</button>
				{props.skippable && <SkipAuthButton onClick={props.onSkip} />}
			</div>
		</div>
	);
}

function PasswordMethodForm(props: AuthMethodViewProps): VNode {
	return (
		<form onSubmit={props.onPassword} className="flex flex-col gap-3">
			<label htmlFor="onboarding-password" className="text-xs text-[var(--muted)] mb-1 block">
				Password{props.localhostOnly ? "" : " *"}
				<input
					id="onboarding-password"
					type="password"
					name="password"
					autoComplete="new-password"
					className="provider-key-input w-full"
					value={props.password}
					onInput={(event) => props.setPassword(targetValue(event))}
					placeholder={props.localhostOnly ? "Optional on localhost" : "At least 12 characters"}
					autofocus
				/>
			</label>
			<label htmlFor="onboarding-password-confirm" className="text-xs text-[var(--muted)] mb-1 block">
				Confirm password
				<input
					id="onboarding-password-confirm"
					type="password"
					name="confirm_password"
					autoComplete="new-password"
					className="provider-key-input w-full"
					value={props.confirmation}
					onInput={(event) => props.setConfirmation(targetValue(event))}
					placeholder="Repeat password"
				/>
			</label>
			{props.error && <ErrorPanel message={props.error} />}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button type="submit" className="provider-btn" disabled={props.saving}>
					{props.saving ? "Setting up\u2026" : props.localhostOnly && !props.password ? "Skip" : "Set password"}
				</button>
				{props.skippable && <SkipAuthButton onClick={props.onSkip} />}
			</div>
		</form>
	);
}

function AuthMethodForm(props: AuthMethodViewProps): VNode | null {
	if (props.method === "passkey") return <PasskeyMethodForm {...props} />;
	if (props.method === "password") return <PasswordMethodForm {...props} />;
	return props.skippable ? <SkipAuthButton onClick={props.onSkip} /> : null;
}

function AuthMethodSelection(
	props: AuthMethodViewProps & {
		localhostOnly: boolean;
		codeRequired: boolean;
		setupCode: string;
		setSetupCode: (value: string) => void;
	},
): VNode {
	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:auth.secureYourInstance")}</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				{props.localhostOnly
					? "Choose how to secure your instance, or skip for now. Setting a password also enables the encryption vault, which protects API keys and secrets stored in the database."
					: "Choose how to secure your instance."}
			</p>
			{props.codeRequired && <SetupCodeField value={props.setupCode} onInput={props.setSetupCode} />}
			<div className="flex flex-col gap-2">
				<button
					type="button"
					className={`backend-card ${props.method === "passkey" ? "selected" : ""} ${props.passkeyEnabled ? "" : "disabled"}`}
					disabled={!props.passkeyEnabled}
					onClick={() => props.setMethod("passkey")}
				>
					<span className="flex flex-wrap items-center justify-between gap-2">
						<span className="text-sm font-medium text-[var(--text)]">Passkey</span>
						<span className="flex flex-wrap gap-2 justify-end">
							{props.passkeyEnabled && <span className="recommended-badge">Recommended</span>}
							{props.passkeyDisabledReason && <span className="tier-badge">{props.passkeyDisabledReason}</span>}
						</span>
					</span>
					<span className="text-xs text-[var(--muted)] mt-1">Use Touch ID, Face ID, or a security key</span>
				</button>
				<button
					type="button"
					className={`backend-card ${props.method === "password" ? "selected" : ""}`}
					onClick={() => props.setMethod("password")}
				>
					<span className="text-sm font-medium text-[var(--text)]">Password</span>
					<span className="text-xs text-[var(--muted)] mt-1">
						Set a password and enable the encryption vault for stored secrets
					</span>
				</button>
			</div>
			<AuthMethodForm {...props} />
		</div>
	);
}

export function AuthStep({ onNext, skippable }: { onNext: () => void; skippable: boolean }): VNode {
	const [method, setMethod] = useState<AuthMethod>(null);
	const [password, setPassword] = useState("");
	const [confirm, setConfirm] = useState("");
	const [setupCode, setSetupCode] = useState("");
	const [passkeyName, setPasskeyName] = useState("");
	const [codeRequired, setCodeRequired] = useState(false);
	const [localhostOnly, setLocalhostOnly] = useState(false);
	const [webauthnAvailable, setWebauthnAvailable] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [saving, setSaving] = useState(false);
	const [loading, setLoading] = useState(true);
	const [passkeyOrigins, setPasskeyOrigins] = useState<string[]>([]);
	const [passkeyDone, setPasskeyDone] = useState(false);
	const [optPw, setOptPw] = useState("");
	const [optPwConfirm, setOptPwConfirm] = useState("");
	const [optPwSaving, setOptPwSaving] = useState(false);
	const [recoveryKey, setRecoveryKey] = useState<string | null>(null);
	const [recoveryCopied, setRecoveryCopied] = useState(false);

	const isIpAddress = /^\d+\.\d+\.\d+\.\d+$/.test(location.hostname) || location.hostname.startsWith("[");
	const browserSupportsWebauthn = !!window.PublicKeyCredential;
	const passkeyEnabled = webauthnAvailable && browserSupportsWebauthn && !isIpAddress;

	const [setupComplete, setSetupComplete] = useState(false);

	useEffect(() => {
		fetch("/api/auth/status")
			.then((r) => r.json())
			.then((data: AuthStatusPayload) => {
				if (data.setup_code_required) setCodeRequired(true);
				if (data.localhost_only) setLocalhostOnly(true);
				if (data.webauthn_available) setWebauthnAvailable(true);
				if (data.passkey_origins) setPasskeyOrigins(data.passkey_origins);
				if (data.setup_complete) setSetupComplete(true);
				setLoading(false);
			})
			.catch(() => setLoading(false));
	}, []);

	// Pre-select passkey when available (easier than passwords)
	useEffect(() => {
		if (passkeyEnabled && method === null) setMethod("passkey");
	}, [passkeyEnabled]);

	function continueWithWebSocket(): void {
		ensureWsConnected();
		onNext();
	}

	async function onPasswordSubmit(event: Event): Promise<void> {
		event.preventDefault();
		setError(null);
		const validationError = passwordSetupError(password, confirm, localhostOnly, codeRequired, setupCode);
		if (validationError) {
			setError(validationError);
			return;
		}
		setSaving(true);
		const result = await submitPasswordSetup(password, setupCode, codeRequired);
		if (!result.ok) {
			setError(result.error);
			setSaving(false);
			return;
		}
		ensureWsConnected();
		if (result.recoveryKey) {
			setRecoveryKey(result.recoveryKey);
			setSaving(false);
		} else onNext();
	}

	async function onPasskeyRegister(): Promise<void> {
		setError(null);
		const validationError = setupCodeError(codeRequired, setupCode);
		if (validationError) {
			setError(validationError);
			return;
		}
		setSaving(true);
		const result = await registerPasskey(passkeyName, codeRequired ? setupCode.trim() : null);
		setSaving(false);
		if (!result.ok) {
			setError(result.error);
			return;
		}
		ensureWsConnected();
		setPasskeyDone(true);
	}

	async function onOptionalPassword(event: Event): Promise<void> {
		event.preventDefault();
		setError(null);
		const validationError = optionalPasswordError(optPw, optPwConfirm);
		if (validationError) {
			setError(validationError);
			return;
		}
		setOptPwSaving(true);
		const result = await submitOptionalPassword(optPw);
		if (result.ok) continueWithWebSocket();
		else {
			setError(result.error);
			setOptPwSaving(false);
		}
	}

	function copyRecoveryKey(): void {
		copyToClipboard(recoveryKey ?? "", "", "Could not copy — please select and copy the key manually.").then((ok) => {
			if (!ok) return;
			setRecoveryCopied(true);
			setTimeout(() => setRecoveryCopied(false), 2000);
		});
	}

	if (loading) {
		return <div className="text-sm text-[var(--muted)]">Checking authentication{"\u2026"}</div>;
	}

	if (setupComplete) return <AuthConfiguredView onNext={continueWithWebSocket} />;
	if (recoveryKey) {
		return (
			<RecoveryKeyView recoveryKey={recoveryKey} copied={recoveryCopied} onCopy={copyRecoveryKey} onNext={onNext} />
		);
	}
	if (passkeyDone) {
		return (
			<OptionalPasswordView
				password={optPw}
				confirmation={optPwConfirm}
				setPassword={setOptPw}
				setConfirmation={setOptPwConfirm}
				saving={optPwSaving}
				error={error}
				onSubmit={onOptionalPassword}
				onSkip={continueWithWebSocket}
			/>
		);
	}
	const disabledReason = passkeyDisabledReason(webauthnAvailable, browserSupportsWebauthn, isIpAddress);
	const originsHint =
		passkeyOrigins.length > 1 ? passkeyOrigins.map((origin) => origin.replace(/^https?:\/\//, "")).join(", ") : null;
	return (
		<AuthMethodSelection
			method={method}
			setMethod={setMethod}
			passkeyEnabled={passkeyEnabled}
			passkeyDisabledReason={disabledReason}
			passkeyName={passkeyName}
			setPasskeyName={setPasskeyName}
			originsHint={originsHint}
			password={password}
			setPassword={setPassword}
			confirmation={confirm}
			setConfirmation={setConfirm}
			localhostOnly={localhostOnly}
			saving={saving}
			error={error}
			skippable={skippable}
			onPasskey={onPasskeyRegister}
			onPassword={onPasswordSubmit}
			onSkip={onNext}
			codeRequired={codeRequired}
			setupCode={setupCode}
			setSetupCode={setSetupCode}
		/>
	);
}
