// ── Security section ─────────────────────────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { DangerZone, EmptyState, ListItem, Loading } from "../../components/forms/ListItem";
import { refresh as refreshGon } from "../../gon";
import { detectPasskeyName } from "../../passkey-detect";
import { targetValue } from "../../typed-events";
import { copyToClipboard } from "../../ui";
import { prepareCreationOptions } from "../../webauthn-helpers";
import { rerender } from "./_shared";

// ── b64/buf helpers (used by passkey registration) ──────────

export function bufToB64(buf: ArrayBuffer): string {
	const bytes = new Uint8Array(buf);
	let str = "";
	for (const b of bytes) str += String.fromCharCode(b);
	return btoa(str).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

interface PasskeyEntry {
	id: string;
	name: string;
	created_at?: string;
}

interface ApiKeyEntry {
	id: string;
	label: string;
	key_prefix?: string;
	created_at?: string;
	scopes?: string[];
}

interface AkScopes {
	"operator.read": boolean;
	"operator.write": boolean;
	"operator.approvals": boolean;
	"operator.pairing": boolean;
	[key: string]: boolean;
}

interface AuthStatus {
	auth_disabled?: boolean;
	localhost_only?: boolean;
	has_password?: boolean;
	has_passkeys?: boolean;
	setup_complete?: boolean;
	passkey_origins?: string[];
	passkey_host_update_hosts?: string[];
}

function applyOptionalBoolean(value: boolean | undefined, setter: (next: boolean) => void): void {
	if (typeof value === "boolean") setter(value);
}

function applyOptionalStrings(value: string[] | undefined, setter: (next: string[]) => void): void {
	if (Array.isArray(value)) setter(value);
}

interface PasswordPanelProps {
	hasPassword: boolean;
	currentPassword: string;
	newPassword: string;
	confirmation: string;
	message: string | null;
	error: string | null;
	saving: boolean;
	recoveryKey: string | null;
	recoveryCopied: boolean;
	awaitingReauth: boolean;
	onCurrentPassword: (value: string) => void;
	onNewPassword: (value: string) => void;
	onConfirmation: (value: string) => void;
	onSubmit: (event: Event) => void;
	onCopyRecovery: () => void;
	onContinueToLogin: () => void;
}

function PasswordRecoveryPanel(
	props: Pick<
		PasswordPanelProps,
		"recoveryKey" | "recoveryCopied" | "awaitingReauth" | "onCopyRecovery" | "onContinueToLogin"
	>,
): VNode | null {
	if (!props.recoveryKey) return null;
	return (
		<div className="mt-3 py-3 px-4 rounded-md border border-[var(--border)] bg-[var(--bg)]">
			<div className="text-xs text-[var(--muted)] mb-1">Vault initialized {"\u2014"} save this recovery key</div>
			<code className="select-all break-all block text-[var(--text-strong)] leading-normal text-[.8rem] font-mono">
				{props.recoveryKey}
			</code>
			<div className="flex items-center gap-2 mt-2">
				<button type="button" className="provider-btn provider-btn-secondary" onClick={props.onCopyRecovery}>
					{props.recoveryCopied ? "Copied!" : "Copy"}
				</button>
				{props.awaitingReauth && (
					<button type="button" className="provider-btn" onClick={props.onContinueToLogin}>
						Continue to sign in
					</button>
				)}
			</div>
			<div className="text-xs text-[var(--error)] mt-2">
				This key will not be shown again. You need it to unlock the vault if you forget your password.
			</div>
		</div>
	);
}

function PasswordField({
	label,
	value,
	onInput,
}: {
	label: string;
	value: string;
	onInput: (value: string) => void;
}): VNode {
	return (
		<label className="flex flex-col gap-1 text-xs text-[var(--muted)]">
			{label}
			<input
				type="password"
				className="provider-key-input w-full"
				value={value}
				onInput={(event) => onInput(targetValue(event))}
			/>
		</label>
	);
}

function passwordButtonLabel(hasPassword: boolean, saving: boolean): string {
	if (saving) return hasPassword ? "Changing\u2026" : "Setting\u2026";
	return hasPassword ? "Change password" : "Set password";
}

function PasswordPanel(props: PasswordPanelProps): VNode {
	return (
		<div className="max-w-form">
			<h3 className="text-sm font-medium text-[var(--text-strong)] mb-2">
				{props.hasPassword ? "Change Password" : "Set Password"}
			</h3>
			<form onSubmit={props.onSubmit}>
				<div className="flex flex-col gap-2 mb-2.5">
					{props.hasPassword && (
						<PasswordField label="Current password" value={props.currentPassword} onInput={props.onCurrentPassword} />
					)}
					<PasswordField
						label={props.hasPassword ? "New password" : "Password"}
						value={props.newPassword}
						onInput={props.onNewPassword}
					/>
					<PasswordField
						label={`Confirm ${props.hasPassword ? "new " : ""}password`}
						value={props.confirmation}
						onInput={props.onConfirmation}
					/>
				</div>
				<div className="flex items-center gap-2">
					<button type="submit" className="provider-btn" disabled={props.saving}>
						{passwordButtonLabel(props.hasPassword, props.saving)}
					</button>
					{props.message && <span className="text-xs text-[var(--accent)]">{props.message}</span>}
					{props.error && <span className="text-xs text-[var(--error)]">{props.error}</span>}
				</div>
			</form>
			<PasswordRecoveryPanel {...props} />
		</div>
	);
}

interface PasskeyPanelProps {
	hasPasskeys: boolean;
	origins: string[];
	hostUpdateHosts: string[];
	loading: boolean;
	passkeys: PasskeyEntry[];
	name: string;
	message: string | null;
	editingId: string | null;
	editingName: string;
	onName: (value: string) => void;
	onEditingName: (value: string) => void;
	onAdd: () => void;
	onStartRename: (id: string, name: string) => void;
	onCancelRename: () => void;
	onConfirmRename: (id: string) => void;
	onRemove: (id: string) => void;
}

function PasskeyRow({ passkey, props }: { passkey: PasskeyEntry; props: PasskeyPanelProps }): VNode {
	if (props.editingId === passkey.id) {
		return (
			<div className="provider-item mb-0">
				<form
					className="flex items-center gap-1.5 flex-1"
					onSubmit={(event) => {
						event.preventDefault();
						props.onConfirmRename(passkey.id);
					}}
				>
					<input
						type="text"
						className="provider-key-input flex-1"
						value={props.editingName}
						onInput={(event) => props.onEditingName(targetValue(event))}
					/>
					<button type="submit" className="provider-btn provider-btn-sm">
						Save
					</button>
					<button
						type="button"
						className="provider-btn provider-btn-sm provider-btn-secondary"
						onClick={props.onCancelRename}
					>
						Cancel
					</button>
				</form>
			</div>
		);
	}
	return (
		<ListItem
			name={passkey.name}
			meta={<time dateTime={passkey.created_at}>{passkey.created_at}</time>}
			actions={[
				<button
					type="button"
					key="rename"
					className="provider-btn provider-btn-sm provider-btn-secondary"
					onClick={() => props.onStartRename(passkey.id, passkey.name)}
				>
					Rename
				</button>,
				<button
					type="button"
					key="remove"
					className="provider-btn provider-btn-sm provider-btn-danger"
					onClick={() => props.onRemove(passkey.id)}
				>
					Remove
				</button>,
			]}
		/>
	);
}

function PasskeyList({ props }: { props: PasskeyPanelProps }): VNode {
	if (props.passkeys.length === 0) return <EmptyState message="No passkeys registered." />;
	return (
		<div className="flex flex-col gap-1.5 mb-3">
			{props.passkeys.map((passkey) => (
				<PasskeyRow key={passkey.id} passkey={passkey} props={props} />
			))}
		</div>
	);
}

function PasskeyPanel(props: PasskeyPanelProps): VNode {
	return (
		<div className="max-w-form border-t border-[var(--border)] pt-4">
			<h3 className="text-sm font-medium text-[var(--text-strong)] mb-2">Passkeys</h3>
			{props.origins.length > 1 && (
				<div className="text-xs text-[var(--muted)] mb-2">
					Passkeys will work when visiting:{" "}
					{props.origins.map((origin) => origin.replace(/^https?:\/\//, "")).join(", ")}
				</div>
			)}
			{props.hasPasskeys && props.hostUpdateHosts.length > 0 && (
				<div className="alert-warning-text max-w-form mb-2">
					<span className="alert-label-warning">Passkey update needed: </span>
					New host detected ({props.hostUpdateHosts.join(", ")}). Sign in with your password on that host, then register
					a new passkey there.
				</div>
			)}
			{props.loading ? (
				<Loading />
			) : (
				<>
					<PasskeyList props={props} />
					<div className="flex gap-2 items-center">
						<input
							type="text"
							className="provider-key-input flex-1"
							value={props.name}
							onInput={(event) => props.onName(targetValue(event))}
							placeholder="Passkey name (e.g. MacBook Touch ID)"
						/>
						<button type="button" className="provider-btn" onClick={props.onAdd}>
							Add passkey
						</button>
					</div>
					{props.message && <div className="text-xs text-[var(--muted)] mt-1.5">{props.message}</div>}
				</>
			)}
		</div>
	);
}

const API_SCOPE_DESCRIPTIONS: Record<string, string> = {
	"operator.read": "View data and status",
	"operator.write": "Create, update, delete",
	"operator.approvals": "Handle command approvals",
	"operator.pairing": "Device/node pairing",
};

interface ApiKeysPanelProps {
	loading: boolean;
	apiKeys: ApiKeyEntry[];
	newKey: string | null;
	label: string;
	fullAccess: boolean;
	scopes: AkScopes;
	onLabel: (value: string) => void;
	onFullAccess: () => void;
	onToggleScope: (scope: string) => void;
	onCreate: () => void;
	onRevoke: (id: string) => void;
}

function NewApiKey({ value }: { value: string | null }): VNode | null {
	if (!value) return null;
	return (
		<div className="mb-3 py-2.5 px-3 bg-[var(--bg)] border border-[var(--border)] rounded-md">
			<div className="text-xs text-[var(--muted)] mb-1">Copy this key now. It won't be shown again.</div>
			<code className="font-mono text-[.78rem] break-all text-[var(--text-strong)]">{value}</code>
		</div>
	);
}

function ApiKeyMeta({ apiKey }: { apiKey: ApiKeyEntry }): VNode {
	return (
		<span className="flex gap-3 flex-wrap">
			<span className="font-mono">{apiKey.key_prefix}...</span>
			<time dateTime={apiKey.created_at}>{apiKey.created_at}</time>
			<span className="text-[var(--accent)]">{apiKey.scopes ? apiKey.scopes.join(", ") : "Full access"}</span>
		</span>
	);
}

function ApiKeyList({ apiKeys, onRevoke }: Pick<ApiKeysPanelProps, "apiKeys" | "onRevoke">): VNode {
	if (apiKeys.length === 0) return <EmptyState message="No API keys." />;
	return (
		<div className="flex flex-col gap-1.5 mb-3">
			{apiKeys.map((apiKey) => (
				<ListItem
					key={apiKey.id}
					name={apiKey.label}
					meta={<ApiKeyMeta apiKey={apiKey} />}
					actions={
						<button type="button" className="provider-btn provider-btn-danger" onClick={() => onRevoke(apiKey.id)}>
							Revoke
						</button>
					}
				/>
			))}
		</div>
	);
}

function ApiScopeSelector({ scopes, onToggle }: { scopes: AkScopes; onToggle: (scope: string) => void }): VNode {
	return (
		<div className="pl-5 flex flex-col gap-1.5">
			<div className="text-xs text-[var(--muted)] mb-0.5">Select permissions:</div>
			{Object.entries(API_SCOPE_DESCRIPTIONS).map(([scope, description]) => (
				<label key={scope} className="flex items-center gap-1.5 cursor-pointer">
					<input type="checkbox" checked={scopes[scope]} onChange={() => onToggle(scope)} />
					<span className="text-xs text-[var(--text)]">{scope}</span>
					<span className="text-xs text-[var(--muted)]">
						{"\u2014"} {description}
					</span>
				</label>
			))}
		</div>
	);
}

function canCreateApiKey(props: ApiKeysPanelProps): boolean {
	return Boolean(props.label.trim() && (props.fullAccess || Object.values(props.scopes).some(Boolean)));
}

function ApiKeysPanel(props: ApiKeysPanelProps): VNode {
	return (
		<div className="max-w-form border-t border-[var(--border)] pt-4">
			<h3 className="text-sm font-medium text-[var(--text-strong)] mb-1">API Keys</h3>
			<p className="text-xs text-[var(--muted)] leading-relaxed mt-0 mb-3">
				API keys authenticate external tools and scripts connecting to chelix over the WebSocket protocol. Pass the key
				as the <code className="font-mono text-xs">api_key</code> field in the{" "}
				<code className="font-mono text-xs">auth</code> object of the <code className="font-mono text-xs">connect</code>{" "}
				handshake.
			</p>
			{props.loading ? (
				<Loading />
			) : (
				<>
					<NewApiKey value={props.newKey} />
					<ApiKeyList apiKeys={props.apiKeys} onRevoke={props.onRevoke} />
					<div className="flex flex-col gap-2.5">
						<input
							type="text"
							className="provider-key-input"
							value={props.label}
							onInput={(event) => props.onLabel(targetValue(event))}
							placeholder="Key label (e.g. CLI tool)"
						/>
						<label className="flex items-center gap-1.5 cursor-pointer">
							<input type="checkbox" checked={props.fullAccess} onChange={props.onFullAccess} />
							<span className="text-xs text-[var(--text)]">Full access (all permissions)</span>
						</label>
						{!props.fullAccess && <ApiScopeSelector scopes={props.scopes} onToggle={props.onToggleScope} />}
						<div>
							<button
								type="button"
								className="provider-btn"
								onClick={props.onCreate}
								disabled={!canCreateApiKey(props)}
							>
								Generate key
							</button>
						</div>
					</div>
				</>
			)}
		</div>
	);
}

function AuthenticationDisabledView(): VNode {
	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Authentication</h2>
			<div className="max-w-form py-3 px-4 rounded-md border border-[var(--error)] bg-[color-mix(in_srgb,var(--error)_5%,transparent)]">
				<strong className="text-[var(--error)]">Authentication is disabled</strong>
				<p className="text-xs text-[var(--muted)] mt-2 mb-0">
					Anyone with network access can control chelix and your computer. Set up a password to protect your instance.
				</p>
				<button type="button" className="provider-btn mt-2.5" onClick={() => window.location.assign("/onboarding")}>
					Set up authentication
				</button>
			</div>
		</div>
	);
}

function SecurityNotices({
	authDisabled,
	localhostOnly,
	hasPassword,
	hasPasskeys,
}: {
	authDisabled: boolean;
	localhostOnly: boolean;
	hasPassword: boolean;
	hasPasskeys: boolean;
}): VNode {
	return (
		<>
			{authDisabled && localhostOnly && (
				<div className="max-w-form py-3 px-4 rounded-md border border-[var(--error)] bg-[color-mix(in_srgb,var(--error)_5%,transparent)]">
					<strong className="text-[var(--error)]">Authentication is disabled</strong>
					<p className="text-xs text-[var(--muted)] mt-2 mb-0">
						Localhost-only access is safe, but localhost bypass is active. Until you add a password or passkey, this
						browser has full access and Sign out has no effect. Add credentials below to require login on localhost and
						before exposing Chelix to your network.
					</p>
				</div>
			)}
			{localhostOnly && !hasPassword && !hasPasskeys && !authDisabled && (
				<div className="alert-info-text max-w-form">
					<span className="alert-label-info">Note: </span>
					Localhost bypass is active. Until you add a password or passkey, this browser has full access and Sign out has
					no effect. Add credentials to require login on localhost and before exposing Chelix to your network.
				</div>
			)}
		</>
	);
}

interface SecurityDangerZoneProps {
	visible: boolean;
	confirming: boolean;
	busy: boolean;
	onReset: () => void;
	onCancel: () => void;
}

function SecurityDangerZone(props: SecurityDangerZoneProps): VNode | null {
	if (!props.visible) return null;
	return (
		<DangerZone>
			<div className="py-3 px-4 border border-[var(--error)] rounded-md bg-[color-mix(in_srgb,var(--error)_5%,transparent)]">
				<strong className="text-sm text-[var(--text-strong)]">Remove all authentication</strong>
				<p className="text-xs text-[var(--muted)] mt-1.5 mb-0">
					If you know what you're doing, you can fully disable authentication. Anyone with network access will be able
					to access chelix and your computer. This removes your password, all passkeys, all API keys, and all sessions.
				</p>
				{props.confirming ? (
					<div className="flex items-center gap-2 mt-2.5">
						<span className="text-xs text-[var(--error)]">Are you sure? This cannot be undone.</span>
						<button
							type="button"
							className="provider-btn provider-btn-danger"
							disabled={props.busy}
							onClick={props.onReset}
						>
							{props.busy ? "Removing\u2026" : "Yes, remove all auth"}
						</button>
						<button type="button" className="provider-btn" onClick={props.onCancel}>
							Cancel
						</button>
					</div>
				) : (
					<button type="button" className="provider-btn provider-btn-danger mt-2.5" onClick={props.onReset}>
						Remove all authentication
					</button>
				)}
			</div>
		</DangerZone>
	);
}

export function SecuritySection(): VNode {
	const [authDisabled, setAuthDisabled] = useState(false);
	const [localhostOnly, setLocalhostOnly] = useState(false);
	const [hasPassword, setHasPassword] = useState(true);
	const [hasPasskeys, setHasPasskeys] = useState(false);
	const [setupComplete, setSetupComplete] = useState(false);
	const [authLoading, setAuthLoading] = useState(true);

	const [curPw, setCurPw] = useState("");
	const [newPw, setNewPw] = useState("");
	const [confirmPw, setConfirmPw] = useState("");
	const [pwMsg, setPwMsg] = useState<string | null>(null);
	const [pwErr, setPwErr] = useState<string | null>(null);
	const [pwSaving, setPwSaving] = useState(false);
	const [pwAwaitingReauth, setPwAwaitingReauth] = useState(false);
	const [pwRecoveryKey, setPwRecoveryKey] = useState<string | null>(null);
	const [pwRecoveryCopied, setPwRecoveryCopied] = useState(false);

	const [passkeys, setPasskeys] = useState<PasskeyEntry[]>([]);
	const [pkName, setPkName] = useState("");
	const [pkMsg, setPkMsg] = useState<string | null>(null);
	const [pkLoading, setPkLoading] = useState(true);
	const [editingPk, setEditingPk] = useState<string | null>(null);
	const [editingPkName, setEditingPkName] = useState("");
	const [passkeyOrigins, setPasskeyOrigins] = useState<string[]>([]);
	const [passkeyHostUpdateHosts, setPasskeyHostUpdateHosts] = useState<string[]>([]);

	const [apiKeys, setApiKeys] = useState<ApiKeyEntry[]>([]);
	const [akLabel, setAkLabel] = useState("");
	const [akNew, setAkNew] = useState<string | null>(null);
	const [akLoading, setAkLoading] = useState(true);
	const [akFullAccess, setAkFullAccess] = useState(true);
	const [akScopes, setAkScopes] = useState<AkScopes>({
		"operator.read": false,
		"operator.write": false,
		"operator.approvals": false,
		"operator.pairing": false,
	});

	function notifyAuthStatusChanged(): void {
		window.dispatchEvent(new CustomEvent("chelix:auth-status-changed"));
	}

	function deferNextPasswordChangedRedirect(): void {
		window.__chelixSuppressNextPasswordChangedRedirect = true;
	}

	function clearPasswordChangedRedirectDeferral(): void {
		window.__chelixSuppressNextPasswordChangedRedirect = false;
	}

	function refreshPasskeyHostStatus(): Promise<void> {
		return fetch("/api/auth/status")
			.then((r) => (r.ok ? r.json() : null))
			.then((status: { passkey_host_update_hosts?: string[]; passkey_origins?: string[] } | null) => {
				if (Array.isArray(status?.passkey_host_update_hosts))
					setPasskeyHostUpdateHosts(status?.passkey_host_update_hosts);
				if (Array.isArray(status?.passkey_origins)) setPasskeyOrigins(status?.passkey_origins);
			});
	}

	function reloadIfAuthNowRequiresLogin({ reload = true } = {}): Promise<boolean> {
		return fetch("/api/auth/status")
			.then((r) => (r.ok ? r.json() : null))
			.then((d: { auth_disabled?: boolean; setup_required?: boolean; authenticated?: boolean } | null) => {
				const mustLogin = !!(d && d.auth_disabled === false && d.setup_required === false && d.authenticated === false);
				if (mustLogin && reload) {
					window.location.reload();
					return true;
				}
				return mustLogin;
			})
			.catch(() => false);
	}

	function applyAuthStatus(status: AuthStatus | null): void {
		applyOptionalBoolean(status?.auth_disabled, setAuthDisabled);
		applyOptionalBoolean(status?.localhost_only, setLocalhostOnly);
		applyOptionalBoolean(status?.has_password, setHasPassword);
		applyOptionalBoolean(status?.has_passkeys, setHasPasskeys);
		applyOptionalBoolean(status?.setup_complete, setSetupComplete);
		applyOptionalStrings(status?.passkey_origins, setPasskeyOrigins);
		applyOptionalStrings(status?.passkey_host_update_hosts, setPasskeyHostUpdateHosts);
		setAuthLoading(false);
		rerender();
	}

	useEffect(() => {
		fetch("/api/auth/status")
			.then((r) => (r.ok ? r.json() : null))
			.then((status: AuthStatus | null) => {
				applyAuthStatus(status);
			})
			.catch(() => {
				setAuthLoading(false);
				rerender();
			});
		fetch("/api/auth/passkeys")
			.then((r) => (r.ok ? r.json() : { passkeys: [] }))
			.then((d: { passkeys?: PasskeyEntry[] }) => {
				setPasskeys(d.passkeys || []);
				setHasPasskeys((d.passkeys || []).length > 0);
				setPkLoading(false);
				rerender();
			})
			.catch(() => setPkLoading(false));
		fetch("/api/auth/api-keys")
			.then((r) => (r.ok ? r.json() : { api_keys: [] }))
			.then((d: { api_keys?: ApiKeyEntry[] }) => {
				setApiKeys(d.api_keys || []);
				setAkLoading(false);
				rerender();
			})
			.catch(() => setAkLoading(false));
	}, []);

	function onChangePw(e: Event): void {
		e.preventDefault();
		setPwErr(null);
		setPwMsg(null);
		if (newPw.length < 12) {
			setPwErr("New password must be at least 12 characters.");
			return;
		}
		if (newPw !== confirmPw) {
			setPwErr("Passwords do not match.");
			return;
		}
		setPwSaving(true);
		setPwAwaitingReauth(false);
		const settingFirstPassword = !hasPassword;
		if (settingFirstPassword) deferNextPasswordChangedRedirect();
		const payload: { new_password: string; current_password?: string } = { new_password: newPw };
		if (hasPassword) payload.current_password = curPw;
		fetch("/api/auth/password/change", {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify(payload),
		})
			.then((r) => {
				if (!r.ok) {
					return r.text().then((t) => {
						clearPasswordChangedRedirectDeferral();
						setPwErr(t);
						setPwSaving(false);
						setPwAwaitingReauth(false);
						rerender();
					});
				}

				return r.json().then((data: { recovery_key?: string }) => {
					const recoveryKey = data.recovery_key;
					const hasRecoveryKey = typeof recoveryKey === "string" && recoveryKey.length > 0;
					setPwMsg(hasPassword ? "Password changed." : "Password set.");
					setCurPw("");
					setNewPw("");
					setConfirmPw("");
					setHasPassword(true);
					setSetupComplete(true);
					setAuthDisabled(false);
					if (recoveryKey) {
						setPwRecoveryKey(recoveryKey);
						refreshGon();
					}
					return reloadIfAuthNowRequiresLogin({ reload: !hasRecoveryKey }).then((requiresLoginOrReloaded) => {
						if (hasRecoveryKey && requiresLoginOrReloaded) {
							setPwAwaitingReauth(true);
							setPwMsg("Password set. Save the recovery key, then continue to sign in.");
							setPwSaving(false);
							rerender();
							return;
						}
						clearPasswordChangedRedirectDeferral();
						setPwAwaitingReauth(false);
						if (!requiresLoginOrReloaded) notifyAuthStatusChanged();
						setPwSaving(false);
						rerender();
					});
				});
			})
			.catch((err: Error) => {
				clearPasswordChangedRedirectDeferral();
				setPwErr(err.message);
				setPwSaving(false);
				setPwAwaitingReauth(false);
				rerender();
			});
	}

	function onAddPasskey(): void {
		setPkMsg(null);
		if (/^\d+\.\d+\.\d+\.\d+$/.test(location.hostname) || location.hostname.startsWith("[")) {
			setPkMsg(`Passkeys require a domain name. Use localhost instead of ${location.hostname}`);
			rerender();
			return;
		}
		let requestedRpId: string | null = null;
		fetch("/api/auth/passkey/register/begin", { method: "POST" })
			.then((r) => r.json())
			.then((data: { options: { publicKey: Record<string, unknown> }; challenge_id: string }) => {
				const pk = data.options.publicKey;
				requestedRpId = (pk.rp as { id?: string })?.id || null;
				const publicKey = prepareCreationOptions(pk);
				return navigator.credentials
					.create({ publicKey })
					.then((cred) => ({ cred: cred as PublicKeyCredential, challengeId: data.challenge_id }));
			})
			.then(({ cred, challengeId }: { cred: PublicKeyCredential; challengeId: string }) => {
				const response = cred.response as AuthenticatorAttestationResponse;
				const body = {
					challenge_id: challengeId,
					name: pkName.trim() || detectPasskeyName(cred),
					credential: {
						id: cred.id,
						rawId: bufToB64(cred.rawId),
						type: cred.type,
						response: {
							attestationObject: bufToB64(response.attestationObject),
							clientDataJSON: bufToB64(response.clientDataJSON),
						},
					},
				};
				return fetch("/api/auth/passkey/register/finish", {
					method: "POST",
					headers: { "Content-Type": "application/json" },
					body: JSON.stringify(body),
				});
			})
			.then((r) => {
				if (r.ok) {
					setPkName("");
					return reloadIfAuthNowRequiresLogin().then((reloaded) => {
						if (reloaded) return;
						return fetch("/api/auth/passkeys")
							.then((r2) => r2.json())
							.then((d: { passkeys?: PasskeyEntry[] }) => {
								setPasskeys(d.passkeys || []);
								setHasPasskeys((d.passkeys || []).length > 0);
								setSetupComplete(true);
								setAuthDisabled(false);
								return refreshPasskeyHostStatus().then(() => {
									setPkMsg("Passkey added.");
									notifyAuthStatusChanged();
									rerender();
								});
							});
					});
				}
				return r.text().then((t) => {
					setPkMsg(t);
					rerender();
				});
			})
			.catch((err: Error) => {
				let msg = err.message || "Failed to add passkey";
				if (requestedRpId) {
					msg += ` (RPID: "${requestedRpId}", current origin: "${location.origin}")`;
				}
				setPkMsg(msg);
				rerender();
			});
	}

	function onStartRename(id: string, currentName: string): void {
		setEditingPk(id);
		setEditingPkName(currentName);
		rerender();
	}

	function onCancelRename(): void {
		setEditingPk(null);
		setEditingPkName("");
		rerender();
	}

	function onConfirmRename(id: string): void {
		const name = editingPkName.trim();
		if (!name) return;
		fetch(`/api/auth/passkeys/${id}`, {
			method: "PATCH",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ name }),
		})
			.then(() => fetch("/api/auth/passkeys").then((r) => r.json()))
			.then((d: { passkeys?: PasskeyEntry[] }) => {
				setPasskeys(d.passkeys || []);
				setEditingPk(null);
				setEditingPkName("");
				rerender();
			});
	}

	function onRemovePasskey(id: string): void {
		fetch(`/api/auth/passkeys/${id}`, { method: "DELETE" })
			.then(() => fetch("/api/auth/passkeys").then((r) => r.json()))
			.then((d: { passkeys?: PasskeyEntry[] }) => {
				setPasskeys(d.passkeys || []);
				setHasPasskeys((d.passkeys || []).length > 0);
				return refreshPasskeyHostStatus().then(() => {
					notifyAuthStatusChanged();
					rerender();
				});
			});
	}

	function onCreateApiKey(): void {
		if (!akLabel.trim()) return;
		setAkNew(null);
		let scopes: string[] | null = null;
		if (!akFullAccess) {
			scopes = Object.entries(akScopes)
				.filter(([, v]) => v)
				.map(([k]) => k);
			if (scopes.length === 0) {
				return;
			}
		}
		fetch("/api/auth/api-keys", {
			method: "POST",
			headers: { "Content-Type": "application/json" },
			body: JSON.stringify({ label: akLabel.trim(), scopes }),
		})
			.then((r) => r.json())
			.then((d: { key?: string }) => {
				setAkNew(d.key || null);
				setAkLabel("");
				setAkFullAccess(true);
				setAkScopes({
					"operator.read": false,
					"operator.write": false,
					"operator.approvals": false,
					"operator.pairing": false,
				});
				return fetch("/api/auth/api-keys").then((r2) => r2.json());
			})
			.then((d: { api_keys?: ApiKeyEntry[] }) => {
				setApiKeys(d.api_keys || []);
				rerender();
			})
			.catch(() => rerender());
	}

	function toggleScope(scope: string): void {
		setAkScopes((prev) => ({ ...prev, [scope]: !prev[scope] }));
		rerender();
	}

	function onRevokeApiKey(id: string): void {
		fetch(`/api/auth/api-keys/${id}`, { method: "DELETE" })
			.then(() => fetch("/api/auth/api-keys").then((r) => r.json()))
			.then((d: { api_keys?: ApiKeyEntry[] }) => {
				setApiKeys(d.api_keys || []);
				rerender();
			});
	}

	const [resetConfirm, setResetConfirm] = useState(false);
	const [resetBusy, setResetBusy] = useState(false);

	function onResetAuth(): void {
		if (!resetConfirm) {
			setResetConfirm(true);
			rerender();
			return;
		}
		setResetBusy(true);
		rerender();
		fetch("/api/auth/reset", { method: "POST" })
			.then((r) => {
				if (r.ok) {
					window.location.reload();
				} else {
					return r.text().then((t) => {
						setPwErr(t);
						setResetConfirm(false);
						setResetBusy(false);
						rerender();
					});
				}
			})
			.catch((err: Error) => {
				setPwErr(err.message);
				setResetConfirm(false);
				setResetBusy(false);
				rerender();
			});
	}

	function onCopyRecoveryKey(): void {
		if (!pwRecoveryKey) return;
		copyToClipboard(pwRecoveryKey, "", "Could not copy — please select and copy the key manually.").then((copied) => {
			if (!copied) return;
			setPwRecoveryCopied(true);
			rerender();
			setTimeout(() => {
				setPwRecoveryCopied(false);
				rerender();
			}, 2000);
		});
	}

	function continueToLogin(): void {
		clearPasswordChangedRedirectDeferral();
		window.location.assign("/login");
	}

	function cancelAuthReset(): void {
		setResetConfirm(false);
		rerender();
	}

	if (authLoading) {
		return (
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Authentication</h2>
				<Loading />
			</div>
		);
	}
	if (authDisabled && !localhostOnly) return <AuthenticationDisabledView />;

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Authentication</h2>
			<SecurityNotices
				authDisabled={authDisabled}
				localhostOnly={localhostOnly}
				hasPassword={hasPassword}
				hasPasskeys={hasPasskeys}
			/>
			<PasswordPanel
				hasPassword={hasPassword}
				currentPassword={curPw}
				newPassword={newPw}
				confirmation={confirmPw}
				message={pwMsg}
				error={pwErr}
				saving={pwSaving}
				recoveryKey={pwRecoveryKey}
				recoveryCopied={pwRecoveryCopied}
				awaitingReauth={pwAwaitingReauth}
				onCurrentPassword={setCurPw}
				onNewPassword={setNewPw}
				onConfirmation={setConfirmPw}
				onSubmit={onChangePw}
				onCopyRecovery={onCopyRecoveryKey}
				onContinueToLogin={continueToLogin}
			/>
			<PasskeyPanel
				hasPasskeys={hasPasskeys}
				origins={passkeyOrigins}
				hostUpdateHosts={passkeyHostUpdateHosts}
				loading={pkLoading}
				passkeys={passkeys}
				name={pkName}
				message={pkMsg}
				editingId={editingPk}
				editingName={editingPkName}
				onName={setPkName}
				onEditingName={setEditingPkName}
				onAdd={onAddPasskey}
				onStartRename={onStartRename}
				onCancelRename={onCancelRename}
				onConfirmRename={onConfirmRename}
				onRemove={onRemovePasskey}
			/>
			<ApiKeysPanel
				loading={akLoading}
				apiKeys={apiKeys}
				newKey={akNew}
				label={akLabel}
				fullAccess={akFullAccess}
				scopes={akScopes}
				onLabel={setAkLabel}
				onFullAccess={() => {
					setAkFullAccess(!akFullAccess);
					rerender();
				}}
				onToggleScope={toggleScope}
				onCreate={onCreateApiKey}
				onRevoke={onRevokeApiKey}
			/>
			<SecurityDangerZone
				visible={setupComplete}
				confirming={resetConfirm}
				busy={resetBusy}
				onReset={onResetAuth}
				onCancel={cancelAuthReset}
			/>
		</div>
	);
}
