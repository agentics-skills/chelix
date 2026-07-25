// ── Environment section ──────────────────────────────────────

import type { ComponentChildren, VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
	Badge,
	CheckboxField,
	EmptyState,
	ListItem,
	Loading,
	SectionHeading,
	StatusMessage,
	SubHeading,
	useSaveState,
} from "../../components/forms";
import * as gon from "../../gon";
import { localizedApiErrorMessage } from "../../helpers";
import { targetValue } from "../../typed-events";
import type { VaultStatus } from "../../types/gon";
import { rerender } from "./_shared";

interface EnvVar {
	id: number;
	key: string;
	value: string | null;
	secret: boolean;
	enabled: boolean;
	encrypted: boolean;
	updated_at: string;
}

interface EnvListResponse {
	env_vars: EnvVar[];
}

function isEnvListResponse(value: unknown): value is EnvListResponse {
	return (
		typeof value === "object" &&
		value !== null &&
		"env_vars" in value &&
		Array.isArray(value.env_vars) &&
		value.env_vars.every(
			(variable) =>
				typeof variable === "object" &&
				variable !== null &&
				typeof variable.id === "number" &&
				typeof variable.key === "string" &&
				typeof variable.secret === "boolean" &&
				(variable.secret ? variable.value === null : typeof variable.value === "string") &&
				typeof variable.enabled === "boolean" &&
				typeof variable.encrypted === "boolean" &&
				typeof variable.updated_at === "string",
		)
	);
}

function errorMessage(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

async function responseError(response: Response, fallback: string): Promise<Error> {
	try {
		const body: unknown = await response.json();
		return new Error(localizedApiErrorMessage(body as Parameters<typeof localizedApiErrorMessage>[0], fallback));
	} catch {
		return new Error(`${fallback} (${response.status})`);
	}
}

function envBadges(variable: EnvVar): VNode[] {
	const badges = [
		variable.secret ? <Badge label="Secret" variant="warning" /> : <Badge label="Visible" variant="configured" />,
		variable.encrypted ? <Badge label="Encrypted" variant="configured" /> : <Badge label="Plaintext" />,
	];
	if (!variable.enabled) badges.push(<Badge label="Disabled" />);
	return badges;
}

interface EnvVarMetaProps {
	variable: EnvVar;
}

function EnvVarMeta({ variable }: EnvVarMetaProps): VNode {
	return (
		<span className="flex flex-wrap gap-3">
			{variable.secret ? (
				<span>{"\u2022\u2022\u2022\u2022\u2022\u2022\u2022\u2022"}</span>
			) : (
				<code className="font-mono text-xs">{variable.value === "" ? "(empty)" : variable.value}</code>
			)}
			<time dateTime={variable.updated_at}>{variable.updated_at}</time>
		</span>
	);
}

interface EnvVarActionsProps {
	variable: EnvVar;
	pending: boolean;
	onUpdateFlags: (variable: EnvVar, secret: boolean, enabled: boolean) => void;
	onStartUpdate: (id: number) => void;
	onDelete: (id: number) => void;
}

function EnvVarActions({ variable, pending, onUpdateFlags, onStartUpdate, onDelete }: EnvVarActionsProps): VNode {
	return (
		<div className="flex flex-wrap items-center justify-end gap-2">
			<CheckboxField
				id={`env-${variable.id}-secret`}
				label="Secret"
				checked={variable.secret}
				disabled={pending}
				onChange={(checked) => onUpdateFlags(variable, checked, variable.enabled)}
				className="flex items-center gap-1 text-xs text-[var(--text)] cursor-pointer"
			/>
			<CheckboxField
				id={`env-${variable.id}-enabled`}
				label="Enabled"
				checked={variable.enabled}
				disabled={pending}
				onChange={(checked) => onUpdateFlags(variable, variable.secret, checked)}
				className="flex items-center gap-1 text-xs text-[var(--text)] cursor-pointer"
			/>
			<button
				type="button"
				className="provider-btn provider-btn-sm"
				disabled={pending}
				onClick={() => onStartUpdate(variable.id)}
			>
				Update
			</button>
			<button
				type="button"
				className="provider-btn provider-btn-sm provider-btn-danger"
				disabled={pending}
				onClick={() => onDelete(variable.id)}
			>
				Delete
			</button>
		</div>
	);
}

interface EnvValueUpdateFormProps {
	variable: EnvVar;
	value: string;
	pending: boolean;
	onValueChange: (value: string) => void;
	onConfirm: (variable: EnvVar) => void;
	onCancel: () => void;
}

function EnvValueUpdateForm({
	variable,
	value,
	pending,
	onValueChange,
	onConfirm,
	onCancel,
}: EnvValueUpdateFormProps): VNode {
	return (
		<form
			className="flex flex-wrap items-center gap-2 mt-2"
			onSubmit={(event: Event) => {
				event.preventDefault();
				onConfirm(variable);
			}}
		>
			<input
				type={variable.secret ? "password" : "text"}
				className="provider-key-input flex-1 min-w-48"
				name="env_update_value"
				autoComplete={variable.secret ? "new-password" : "off"}
				autoCorrect="off"
				autoCapitalize="off"
				spellcheck={false}
				value={value}
				onInput={(event: Event) => onValueChange(targetValue(event))}
				placeholder="New value"
			/>
			<button type="submit" className="provider-btn" disabled={pending}>
				Save
			</button>
			<button type="button" className="provider-btn" onClick={onCancel}>
				Cancel
			</button>
		</form>
	);
}

interface EnvVarRowProps {
	variable: EnvVar;
	pending: boolean;
	updating: boolean;
	updateValue: string;
	onUpdateValueChange: (value: string) => void;
	onUpdateFlags: (variable: EnvVar, secret: boolean, enabled: boolean) => void;
	onStartUpdate: (id: number) => void;
	onConfirmUpdate: (variable: EnvVar) => void;
	onCancelUpdate: () => void;
	onDelete: (id: number) => void;
}

function EnvVarRow({
	variable,
	pending,
	updating,
	updateValue,
	onUpdateValueChange,
	onUpdateFlags,
	onStartUpdate,
	onConfirmUpdate,
	onCancelUpdate,
	onDelete,
}: EnvVarRowProps): VNode {
	return (
		<ListItem
			className={variable.enabled ? undefined : "opacity-60"}
			name={<span className="font-mono text-xs">{variable.key}</span>}
			badges={envBadges(variable)}
			meta={<EnvVarMeta variable={variable} />}
			actions={
				<EnvVarActions
					variable={variable}
					pending={pending}
					onUpdateFlags={onUpdateFlags}
					onStartUpdate={onStartUpdate}
					onDelete={onDelete}
				/>
			}
		>
			{updating ? (
				<EnvValueUpdateForm
					variable={variable}
					value={updateValue}
					pending={pending}
					onValueChange={onUpdateValueChange}
					onConfirm={onConfirmUpdate}
					onCancel={onCancelUpdate}
				/>
			) : undefined}
		</ListItem>
	);
}

interface EnvVaultNoticeProps {
	vaultStatus: VaultStatus | null;
	authHasPassword: boolean;
}

function EnvVaultNotice({ vaultStatus, authHasPassword }: EnvVaultNoticeProps): VNode | null {
	if (!vaultStatus || vaultStatus === "disabled") return null;

	return (
		<div
			className="text-xs"
			style={{
				maxWidth: "600px",
				padding: "8px 12px",
				borderRadius: "6px",
				border: "1px solid var(--border)",
				background: "var(--bg)",
			}}
		>
			{vaultStatus === "unsealed" ? (
				<>
					<span style={{ color: "var(--accent)" }}>Vault unlocked.</span> Your keys are stored encrypted.
				</>
			) : vaultStatus === "sealed" ? (
				<>
					<span style={{ color: "var(--warning,var(--error))" }}>Vault locked.</span> Encrypted keys can{"\u2019"}t be
					read {"\u2014"} sandbox commands won{"\u2019"}t work.{" "}
					<a href="/settings/vault" style={{ color: "inherit", textDecoration: "underline" }}>
						Unlock in Encryption settings.
					</a>
				</>
			) : (
				<>
					<span className="text-[var(--muted)]">Vault not set up.</span>{" "}
					{authHasPassword ? (
						<>
							<a href="/settings/vault" style={{ color: "inherit", textDecoration: "underline" }}>
								Initialize the vault
							</a>{" "}
							to encrypt your stored keys.
						</>
					) : (
						<>
							<a href="/settings/security" style={{ color: "inherit", textDecoration: "underline" }}>
								Set a password
							</a>{" "}
							to encrypt your stored keys.
						</>
					)}
				</>
			)}
		</div>
	);
}

interface EnvironmentLoadStateProps {
	loading: boolean;
	loaded: boolean;
	children: ComponentChildren;
}

function EnvironmentLoadState({ loading, loaded, children }: EnvironmentLoadStateProps): VNode | null {
	if (loading) return <Loading />;
	if (!loaded) return null;
	return <>{children}</>;
}

export function EnvironmentSection(): VNode {
	const [envVars, setEnvVars] = useState<EnvVar[]>([]);
	const [envLoading, setEnvLoading] = useState(true);
	const [envLoaded, setEnvLoaded] = useState(false);
	const [newKey, setNewKey] = useState("");
	const [newValue, setNewValue] = useState("");
	const [newSecret, setNewSecret] = useState(true);
	const [newEnabled, setNewEnabled] = useState(true);
	const save = useSaveState();
	const [updateId, setUpdateId] = useState<number | null>(null);
	const [updateValue, setUpdateValue] = useState("");
	const [pendingId, setPendingId] = useState<number | null>(null);
	const [envError, setEnvError] = useState<string | null>(null);

	async function fetchEnvVars(): Promise<void> {
		const response = await fetch("/api/env");
		if (!response.ok) throw await responseError(response, "Failed to load environment variables");
		const data: unknown = await response.json();
		if (!isEnvListResponse(data)) throw new Error("Invalid environment variable response");
		setEnvVars(data.env_vars);
		setEnvLoaded(true);
		setEnvError(null);
		setEnvLoading(false);
		rerender();
	}

	useEffect(() => {
		void fetchEnvVars().catch((error: unknown) => {
			setEnvError(errorMessage(error));
			setEnvLoading(false);
			rerender();
		});
	}, []);

	async function onAdd(e: Event): Promise<void> {
		e.preventDefault();
		save.reset();
		const key = newKey.trim();
		if (!key) {
			save.setError("Key is required.");
			rerender();
			return;
		}
		if (!/^[A-Za-z0-9_]+$/.test(key)) {
			save.setError("Key must contain only letters, digits, and underscores.");
			rerender();
			return;
		}
		save.setSaving(true);
		rerender();
		try {
			const response = await fetch("/api/env", {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ key, value: newValue, secret: newSecret, enabled: newEnabled }),
			});
			if (!response.ok) throw await responseError(response, "Failed to save environment variable");
			setNewKey("");
			setNewValue("");
			setNewSecret(true);
			setNewEnabled(true);
			await fetchEnvVars();
			save.flashSaved();
		} catch (error: unknown) {
			save.setError(errorMessage(error));
		} finally {
			save.setSaving(false);
			rerender();
		}
	}

	async function onDelete(id: number): Promise<void> {
		setPendingId(id);
		setEnvError(null);
		try {
			const response = await fetch(`/api/env/${id}`, { method: "DELETE" });
			if (!response.ok) throw await responseError(response, "Failed to delete environment variable");
			await fetchEnvVars();
		} catch (error: unknown) {
			setEnvError(errorMessage(error));
		} finally {
			setPendingId(null);
			rerender();
		}
	}

	function onStartUpdate(id: number): void {
		setUpdateId(id);
		setUpdateValue("");
		rerender();
	}

	function onCancelUpdate(): void {
		setUpdateId(null);
		setUpdateValue("");
		rerender();
	}

	async function onConfirmUpdate(variable: EnvVar): Promise<void> {
		setPendingId(variable.id);
		setEnvError(null);
		try {
			const response = await fetch("/api/env", {
				method: "POST",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({
					key: variable.key,
					value: updateValue,
					secret: variable.secret,
					enabled: variable.enabled,
				}),
			});
			if (!response.ok) throw await responseError(response, "Failed to update environment variable");
			setUpdateId(null);
			setUpdateValue("");
			await fetchEnvVars();
		} catch (error: unknown) {
			setEnvError(errorMessage(error));
		} finally {
			setPendingId(null);
			rerender();
		}
	}

	async function onUpdateFlags(variable: EnvVar, secret: boolean, enabled: boolean): Promise<void> {
		setPendingId(variable.id);
		setEnvError(null);
		try {
			const response = await fetch(`/api/env/${variable.id}`, {
				method: "PATCH",
				headers: { "Content-Type": "application/json" },
				body: JSON.stringify({ secret, enabled }),
			});
			if (!response.ok) throw await responseError(response, "Failed to update environment variable");
			await fetchEnvVars();
		} catch (error: unknown) {
			setEnvError(errorMessage(error));
		} finally {
			setPendingId(null);
			rerender();
		}
	}

	const envVaultStatus = gon.get("vault_status") ?? null;
	const authHasPassword = gon.get("auth_has_password") === true;

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<SectionHeading title="Environment Variables" />
			<p className="text-xs text-[var(--muted)] leading-relaxed max-w-form m-0">
				Enabled variables are injected into sandbox command execution. Secret values are masked in this list and in
				command output; non-secret values remain visible.
			</p>
			<EnvVaultNotice vaultStatus={envVaultStatus} authHasPassword={authHasPassword} />
			<StatusMessage error={envError} success={null} />

			<EnvironmentLoadState loading={envLoading} loaded={envLoaded}>
				{/* Existing variables */}
				<div className="max-w-form">
					{envVars.length > 0 ? (
						<div className="flex flex-col gap-1.5 mb-3">
							{envVars.map((variable) => (
								<EnvVarRow
									key={variable.id}
									variable={variable}
									pending={pendingId === variable.id}
									updating={updateId === variable.id}
									updateValue={updateValue}
									onUpdateValueChange={setUpdateValue}
									onUpdateFlags={(current, secret, enabled) => void onUpdateFlags(current, secret, enabled)}
									onStartUpdate={onStartUpdate}
									onConfirmUpdate={(current) => void onConfirmUpdate(current)}
									onCancelUpdate={onCancelUpdate}
									onDelete={(id) => void onDelete(id)}
								/>
							))}
						</div>
					) : (
						<EmptyState message="No environment variables set." />
					)}
				</div>

				{/* Add variable */}
				<div className="max-w-form border-t border-[var(--border)] pt-4">
					<SubHeading title="Add Variable" />
					<form aria-label="Add environment variable" onSubmit={(event) => void onAdd(event)}>
						<div className="flex gap-2 flex-wrap">
							<input
								type="text"
								name="env_key"
								autoComplete="off"
								autoCorrect="off"
								autoCapitalize="off"
								spellcheck={false}
								value={newKey}
								onInput={(e: Event) => setNewKey(targetValue(e))}
								placeholder="KEY_NAME"
								className="provider-key-input flex-1 min-w-30 font-mono text-xs"
							/>
							<input
								type={newSecret ? "password" : "text"}
								className="provider-key-input flex-2 min-w-50"
								name="env_value"
								autoComplete={newSecret ? "new-password" : "off"}
								autoCorrect="off"
								autoCapitalize="off"
								spellcheck={false}
								value={newValue}
								onInput={(e: Event) => setNewValue(targetValue(e))}
								placeholder="Value"
							/>
							<button type="submit" className="provider-btn" disabled={save.saving || !newKey.trim()}>
								{save.saving ? "Saving\u2026" : "Add"}
							</button>
						</div>
						<div className="flex flex-wrap gap-4 mt-3">
							<CheckboxField
								id="env-new-secret"
								label="Secret"
								checked={newSecret}
								onChange={setNewSecret}
								help="mask in the list and command output"
							/>
							<CheckboxField
								id="env-new-enabled"
								label="Enabled"
								checked={newEnabled}
								onChange={setNewEnabled}
								help="inject into sandbox commands"
							/>
						</div>
						<StatusMessage error={save.error} success={save.saved ? "Variable saved." : null} />
					</form>
				</div>
			</EnvironmentLoadState>
		</div>
	);
}
