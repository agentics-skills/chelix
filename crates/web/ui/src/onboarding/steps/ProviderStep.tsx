// ── Provider step (provider config, model selection) ─────────

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { providerApiKeyHelp } from "../../provider-key-help";
import {
	humanizeProbeError,
	isModelServiceNotConfigured,
	providerBaseUrlError,
	saveProviderKey,
	testModel,
} from "../../provider-validation";
import { targetValue } from "../../typed-events";
import { ErrorPanel } from "../shared";
import type { KeyHelp, ModelSelectorRow, ProbeResult, ProviderInfo, RawModelRow } from "../types";

// ── Constants ───────────────────────────────────────────────

const OPENAI_COMPATIBLE = ["openai", "openrouter"];
const RECOMMENDED_PROVIDERS = new Set(["openai", "zai"]);

const WS_RETRY_LIMIT = 75;
const WS_RETRY_DELAY_MS = 200;

// ── Helper functions ────────────────────────────────────────

export function sortProviders(list: ProviderInfo[]): ProviderInfo[] {
	list.sort((a, b) => {
		const aOrder = Number.isFinite(a.uiOrder) ? (a.uiOrder as number) : Number.MAX_SAFE_INTEGER;
		const bOrder = Number.isFinite(b.uiOrder) ? (b.uiOrder as number) : Number.MAX_SAFE_INTEGER;
		if (aOrder !== bOrder) return aOrder - bOrder;
		return a.displayName.localeCompare(b.displayName);
	});
	return list;
}

function modelBelongsToProvider(providerName: string, mdl: ModelSelectorRow): boolean {
	return mdl.provider === providerName;
}

function toModelSelectorRow(modelRow: RawModelRow): ModelSelectorRow {
	return modelRow;
}

// ── ModelSelectCard ─────────────────────────────────────────

export function ModelSelectCard({
	model,
	selected,
	probe,
	onToggle,
}: {
	model: ModelSelectorRow;
	selected: boolean;
	probe: string | ProbeResult | undefined;
	onToggle: () => void;
}): VNode {
	const probeError = probe && probe !== "ok" && probe !== "probing" ? (probe as ProbeResult).error || "" : "";
	return (
		<button type="button" className={`model-card ${selected ? "selected" : ""}`} onClick={onToggle}>
			<span className="flex flex-wrap items-center justify-between gap-2">
				<span className="text-sm font-medium text-[var(--text)]">{model.id}</span>
				<span className="flex flex-wrap gap-2 justify-end">
					{model.tool_calling ? <span className="recommended-badge">Tools</span> : null}
					{probe === "probing" ? <span className="tier-badge">Probing{"\u2026"}</span> : null}
					{probeError ? <span className="provider-item-badge warning">Unsupported</span> : null}
				</span>
			</span>
			{probeError ? (
				<span className="text-xs font-medium text-[var(--danger,#ef4444)] mt-0.5">{probeError}</span>
			) : null}
		</button>
	);
}

// ── OnboardingProviderRow ───────────────────────────────────

interface OnboardingProviderRowProps {
	provider: ProviderInfo;
	configuring: string | null;
	phase: string;
	providerModels: ModelSelectorRow[];
	selectedModels: Set<string>;
	probeResults: Map<string, string | ProbeResult>;
	modelSearch: string;
	setModelSearch: (v: string) => void;
	apiKey: string;
	setApiKey: (v: string) => void;
	endpoint: string;
	setEndpoint: (v: string) => void;
	savingModels: boolean;
	error: string | null;
	onStartConfigure: (name: string) => void;
	onCancelConfigure: () => void;
	onSaveKey: (e: Event) => void;
	onToggleModel: (id: string) => void;
	onSaveModels: () => void;
}

function ProviderRowHeader({
	provider,
	expanded,
	onConfigure,
}: {
	provider: ProviderInfo;
	expanded: boolean;
	onConfigure: () => void;
}): VNode {
	return (
		<div className="flex items-center gap-3">
			<div className="flex-1 min-w-0 flex flex-col gap-0.5">
				<div className="flex items-center gap-2 flex-wrap">
					<span className="text-sm font-medium text-[var(--text-strong)]">{provider.displayName}</span>
					{provider.configured && <span className="provider-item-badge configured">configured</span>}
				</div>
			</div>
			{!expanded && (
				<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={onConfigure}>
					{provider.configured ? "Choose Model" : "Configure"}
				</button>
			)}
		</div>
	);
}

interface ProviderApiKeyFormProps {
	provider: ProviderInfo;
	phase: string;
	apiKey: string;
	setApiKey: (value: string) => void;
	endpoint: string;
	setEndpoint: (value: string) => void;
	error: string | null;
	onSave: (event: Event) => void;
	onCancel: () => void;
}

function ProviderApiKeyForm(props: ProviderApiKeyFormProps): VNode {
	const keyInputRef = useRef<HTMLInputElement>(null);
	useEffect(() => keyInputRef.current?.focus(), []);
	const keyHelp = providerApiKeyHelp(props.provider) as KeyHelp | null;
	return (
		<form onSubmit={props.onSave} className="flex flex-col gap-2 mt-3 border-t border-[var(--border)] pt-3">
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">API Key</span>
				<input
					type="password"
					className="provider-key-input w-full"
					ref={keyInputRef}
					value={props.apiKey}
					onInput={(event) => props.setApiKey(targetValue(event))}
					placeholder={props.provider.keyOptional ? "(optional)" : "sk-..."}
				/>
			</label>
			{keyHelp && (
				<div className="text-xs text-[var(--muted)] mt-1">
					{keyHelp.url ? (
						<>
							{keyHelp.text}{" "}
							<a
								href={keyHelp.url}
								target="_blank"
								rel="noopener noreferrer"
								className="text-[var(--accent)] underline"
							>
								{keyHelp.label || keyHelp.url}
							</a>
						</>
					) : (
						keyHelp.text
					)}
				</div>
			)}
			{OPENAI_COMPATIBLE.includes(props.provider.name) && (
				<div>
					<label>
						<span className="text-xs text-[var(--muted)] mb-1 block">Endpoint (optional)</span>
						<input
							type="text"
							className="provider-key-input w-full"
							value={props.endpoint}
							onInput={(event) => props.setEndpoint(targetValue(event))}
							placeholder={props.provider.defaultBaseUrl || "https://api.example.com/v1"}
						/>
					</label>
					<div className="text-xs text-[var(--muted)] mt-1">Leave empty to use the default endpoint.</div>
				</div>
			)}
			{props.error && <ErrorPanel message={props.error} />}
			<div className="flex items-center gap-2 mt-1">
				<button
					key={`prov-${props.phase}`}
					type="submit"
					className="provider-btn provider-btn-sm"
					disabled={props.phase === "saving"}
				>
					{props.phase === "saving" ? "Saving\u2026" : "Save"}
				</button>
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm"
					onClick={props.onCancel}
					disabled={props.phase === "saving"}
				>
					Cancel
				</button>
			</div>
		</form>
	);
}

function sortedProviderModels(models: ModelSelectorRow[]): ModelSelectorRow[] {
	return models.slice().sort((first, second) => first.id.localeCompare(second.id));
}

interface ProviderModelFormProps {
	models: ModelSelectorRow[];
	selectedModels: Set<string>;
	probeResults: Map<string, string | ProbeResult>;
	modelSearch: string;
	setModelSearch: (value: string) => void;
	saving: boolean;
	error: string | null;
	onToggle: (id: string) => void;
	onSave: () => void;
	onCancel: () => void;
}

function ProviderModelForm(props: ProviderModelFormProps): VNode {
	const [showAllModels, setShowAllModels] = useState(false);
	const search = props.modelSearch.toLowerCase();
	const filtered = sortedProviderModels(props.models).filter(
		(model) => !search || model.id.toLowerCase().includes(search),
	);
	const visible = showAllModels || search ? filtered : filtered.slice(0, 3);
	const hasMore = filtered.length > 3 && !search;
	return (
		<div className="flex flex-col gap-2 mt-3 border-t border-[var(--border)] pt-3">
			<div className="text-xs font-medium text-[var(--text-strong)]">Select preferred models</div>
			<div className="text-xs text-[var(--muted)]">Selected models appear first in the session model selector.</div>
			{props.models.length > 5 && (
				<input
					type="text"
					className="provider-key-input w-full text-xs"
					placeholder="Search models\u2026"
					value={props.modelSearch}
					onInput={(event) => props.setModelSearch(targetValue(event))}
				/>
			)}
			<div className="flex flex-col gap-1">
				{visible.length === 0 ? (
					<div className="text-xs text-[var(--muted)] py-4 text-center">No models match your search.</div>
				) : (
					visible.map((model) => (
						<ModelSelectCard
							key={model.id}
							model={model}
							selected={props.selectedModels.has(model.id)}
							probe={props.probeResults.get(model.id)}
							onToggle={() => props.onToggle(model.id)}
						/>
					))
				)}
				{hasMore && (
					<button
						type="button"
						className="text-xs text-[var(--accent)] cursor-pointer bg-transparent border-none py-1 text-left hover:underline"
						onClick={() => setShowAllModels(!showAllModels)}
					>
						{showAllModels
							? t("providers:showFewerModels")
							: t("providers:showAllModels", { count: filtered.length - 3 })}
					</button>
				)}
			</div>
			<div className="text-xs text-[var(--muted)]">
				{props.selectedModels.size === 0
					? "No models selected"
					: `${props.selectedModels.size} model${props.selectedModels.size > 1 ? "s" : ""} selected`}
			</div>
			{props.error && <ErrorPanel message={props.error} />}
			<div className="flex items-center gap-2 mt-1">
				<button
					type="button"
					className="provider-btn provider-btn-sm"
					disabled={props.selectedModels.size === 0 || props.saving}
					onClick={props.onSave}
				>
					{props.saving ? "Saving\u2026" : "Save"}
				</button>
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm"
					onClick={props.onCancel}
					disabled={props.saving}
				>
					Cancel
				</button>
			</div>
			{props.saving && (
				<div className="text-xs text-[var(--muted)] mt-1">Saving selected model preferences{"\u2026"}</div>
			)}
		</div>
	);
}

export function OnboardingProviderRow(props: OnboardingProviderRowProps): VNode {
	const apiKeyForm = props.configuring === props.provider.name && (props.phase === "form" || props.phase === "saving");
	const modelForm = props.configuring === props.provider.name && props.phase === "selectModel";
	const expanded = apiKeyForm || modelForm;
	const rowRef = useRef<HTMLDivElement>(null);
	useEffect(() => {
		if (expanded) rowRef.current?.scrollIntoView({ behavior: "smooth", block: "nearest" });
	}, [expanded]);
	return (
		<div ref={rowRef} className="rounded-md border border-[var(--border)] bg-[var(--surface)] p-3">
			<ProviderRowHeader
				provider={props.provider}
				expanded={expanded}
				onConfigure={() => props.onStartConfigure(props.provider.name)}
			/>
			{apiKeyForm && (
				<ProviderApiKeyForm
					provider={props.provider}
					phase={props.phase}
					apiKey={props.apiKey}
					setApiKey={props.setApiKey}
					endpoint={props.endpoint}
					setEndpoint={props.setEndpoint}
					error={props.error}
					onSave={props.onSaveKey}
					onCancel={props.onCancelConfigure}
				/>
			)}
			{modelForm && (
				<ProviderModelForm
					models={props.providerModels}
					selectedModels={props.selectedModels}
					probeResults={props.probeResults}
					modelSearch={props.modelSearch}
					setModelSearch={props.setModelSearch}
					saving={props.savingModels}
					error={props.error}
					onToggle={props.onToggleModel}
					onSave={props.onSaveModels}
					onCancel={props.onCancelConfigure}
				/>
			)}
		</div>
	);
}

// ── ProviderStep ─────────────────────────────────────────────

export function ProviderStep({ onNext, onBack }: { onNext: () => void; onBack?: (() => void) | null }): VNode {
	const [providers, setProviders] = useState<ProviderInfo[]>([]);
	const [loading, setLoading] = useState(true);
	const [error, setError] = useState<string | null>(null);
	const [showAllProviders, setShowAllProviders] = useState(false);
	const [configuring, setConfiguring] = useState<string | null>(null);
	const [phase, setPhase] = useState("form");
	const [providerModels, setProviderModels] = useState<ModelSelectorRow[]>([]);
	const [selectedModels, setSelectedModels] = useState<Set<string>>(new Set());
	const [probeResults, setProbeResults] = useState<Map<string, string | ProbeResult>>(new Map());
	const [modelSearch, setModelSearch] = useState("");
	const [savingModels, setSavingModels] = useState(false);
	const [modelSelectProvider, setModelSelectProvider] = useState<string | null>(null);
	const [apiKey, setApiKey] = useState("");
	const [endpoint, setEndpoint] = useState("");

	function refreshProviders(): Promise<unknown> {
		return sendRpc<ProviderInfo[]>("providers.available", {}).then((res) => {
			if (res?.ok) setProviders(sortProviders(res.payload || []));
			return res;
		});
	}

	useEffect(() => {
		let cancelled = false;
		let attempts = 0;
		function loadProviders(): void {
			if (cancelled) return;
			sendRpc<ProviderInfo[]>("providers.available", {}).then((res) => {
				if (cancelled) return;
				if (res?.ok) {
					setProviders(sortProviders(res.payload || []));
					setLoading(false);
					return;
				}
				if (
					((res?.error as { code?: string })?.code === "UNAVAILABLE" ||
						(res?.error as { message?: string })?.message === "WebSocket not connected") &&
					attempts < WS_RETRY_LIMIT
				) {
					attempts += 1;
					window.setTimeout(loadProviders, WS_RETRY_DELAY_MS);
					return;
				}
				setLoading(false);
			});
		}
		loadProviders();
		return () => {
			cancelled = true;
		};
	}, []);

	function closeAll(): void {
		setConfiguring(null);
		setModelSelectProvider(null);
		setPhase("form");
		setProviderModels([]);
		setSelectedModels(new Set());
		setProbeResults(new Map());
		setModelSearch("");
		setSavingModels(false);
		setApiKey("");
		setEndpoint("");
		setError(null);
	}

	async function loadModelsForProvider(providerName: string): Promise<ModelSelectorRow[]> {
		const modelsRes = await sendRpc<RawModelRow[]>("models.list", {});
		const allModels = modelsRes?.ok ? modelsRes.payload || [] : [];
		return allModels.filter((m) => modelBelongsToProvider(providerName, toModelSelectorRow(m))).map(toModelSelectorRow);
	}

	async function openModelSelectForConfiguredProvider(provider: ProviderInfo): Promise<boolean> {
		if (!provider.configured) return false;
		const existingModels = await loadModelsForProvider(provider.name);
		if (existingModels.length === 0) return false;
		const saved = new Set(existingModels.filter((model) => model.preferred).map((model) => model.id));
		setModelSelectProvider(provider.name);
		setConfiguring(provider.name);
		setProviderModels(existingModels);
		setSelectedModels(saved);
		setPhase("selectModel");
		return true;
	}

	async function onStartConfigure(name: string): Promise<void> {
		closeAll();
		const p = providers.find((pr) => pr.name === name);
		if (!p) return;
		setEndpoint(p.baseUrl || "");
		if (await openModelSelectForConfiguredProvider(p)) return;
		setConfiguring(name);
		setPhase("form");
	}

	function onSaveKey(e: Event): void {
		e.preventDefault();
		const p = providers.find((pr) => pr.name === configuring);
		if (!p) return;
		if (!(apiKey.trim() || p.keyOptional)) {
			setError("API key is required.");
			return;
		}
		setError(null);
		setPhase("saving");
		const keyVal = apiKey.trim() || p.name;
		const endpointVal = endpoint.trim() || null;
		const endpointError = providerBaseUrlError(endpointVal);
		if (endpointError) {
			setPhase("form");
			setError(endpointError);
			return;
		}

		saveProviderKey(p.name, keyVal, endpointVal)
			.then((saveRes) => {
				if (!saveRes?.ok) {
					setPhase("form");
					setError((saveRes?.error as { message?: string })?.message || "Failed to save credentials.");
					return;
				}
				closeAll();
				refreshProviders();
			})
			.catch((err: Error) => {
				setPhase("form");
				setError(err?.message || "Failed to save credentials.");
			});
	}

	function probeModelAsync(modelId: string): void {
		setProbeResults((prev) => {
			const next = new Map(prev);
			next.set(modelId, "probing");
			return next;
		});
		testModel(modelId).then((result: { ok: boolean; error?: string }) => {
			setProbeResults((prev) => {
				const next = new Map(prev);
				if (isModelServiceNotConfigured(result.error || "")) next.delete(modelId);
				else
					next.set(
						modelId,
						result.ok ? "ok" : { error: humanizeProbeError(result.error || "Unsupported") as string | undefined },
					);
				return next;
			});
		});
	}

	function onToggleModel(modelId: string): void {
		setSelectedModels((prev) => {
			const next = new Set(prev);
			if (next.has(modelId)) next.delete(modelId);
			else {
				next.add(modelId);
				probeModelAsync(modelId);
			}
			return next;
		});
	}

	async function saveSelectedModelPreferences(providerName: string): Promise<string | null> {
		const response = await sendRpc("providers.set_model_preferences", {
			provider: providerName,
			modelIds: Array.from(selectedModels),
		});
		return response?.ok ? null : response?.error?.message || "Failed to save model preferences.";
	}

	function finishSelectedModelSave(): void {
		const firstModelId = selectedModels.values().next().value;
		if (firstModelId) localStorage.setItem("chelix-model", firstModelId);
		closeAll();
		refreshProviders();
	}

	async function onSaveSelectedModels(): Promise<boolean> {
		const providerName = modelSelectProvider || configuring;
		if (!providerName) return false;
		setSavingModels(true);
		setError(null);
		try {
			const errorMessage = await saveSelectedModelPreferences(providerName);
			if (errorMessage) {
				setError(errorMessage);
				return false;
			}
			finishSelectedModelSave();
			return true;
		} catch (err) {
			setError((err as Error)?.message || "Failed to save model preferences.");
			return false;
		} finally {
			setSavingModels(false);
		}
	}

	async function onContinue(): Promise<void> {
		const hasPendingModelSelection =
			phase === "selectModel" && (configuring || modelSelectProvider) && selectedModels.size > 0;
		if (hasPendingModelSelection) {
			const saved = await onSaveSelectedModels();
			if (!saved) return;
		}
		onNext();
	}

	if (loading) return <div className="text-sm text-[var(--muted)]">{t("onboarding:provider.loadingLlms")}</div>;

	const configuredProviders = providers.filter((p) => p.configured);
	const recommendedProviders = providers.filter((p) => p.configured || RECOMMENDED_PROVIDERS.has(p.name));
	const otherProviders = providers.filter((p) => !(p.configured || RECOMMENDED_PROVIDERS.has(p.name)));
	const otherIsActive = otherProviders.some((p) => configuring === p.name);
	const showOther = showAllProviders || otherIsActive;

	function renderProviderRow(p: ProviderInfo): VNode {
		return (
			<OnboardingProviderRow
				key={p.name}
				provider={p}
				configuring={configuring}
				phase={configuring === p.name ? phase : "form"}
				providerModels={configuring === p.name ? providerModels : []}
				selectedModels={configuring === p.name ? selectedModels : new Set()}
				probeResults={configuring === p.name ? probeResults : new Map()}
				modelSearch={configuring === p.name ? modelSearch : ""}
				setModelSearch={setModelSearch}
				apiKey={apiKey}
				setApiKey={setApiKey}
				endpoint={endpoint}
				setEndpoint={setEndpoint}
				savingModels={savingModels}
				error={configuring === p.name ? error : null}
				onStartConfigure={onStartConfigure}
				onCancelConfigure={closeAll}
				onSaveKey={onSaveKey}
				onToggleModel={onToggleModel}
				onSaveModels={onSaveSelectedModels}
			/>
		);
	}

	return (
		<div className="flex flex-col gap-4">
			<div className="flex items-baseline justify-between gap-2">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:provider.addLlms")}</h2>
				<a
					href="https://github.com/agentics-skills/chelix/blob/master/docs/src/choosing-a-provider.md"
					target="_blank"
					rel="noopener noreferrer"
					className="text-xs text-[var(--accent)] hover:underline shrink-0"
				>
					Help me choose
				</a>
			</div>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Configure one or more LLM providers to power your agent. You can add more later in Settings.
			</p>
			{configuredProviders.length > 0 ? (
				<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 flex flex-col gap-2">
					<div className="text-xs text-[var(--muted)]">Configured LLM providers</div>
					<div className="flex flex-wrap gap-2">
						{configuredProviders.map((p) => (
							<span key={p.name} className="provider-item-badge configured">
								{p.displayName}
							</span>
						))}
					</div>
				</div>
			) : null}
			<div className="flex flex-col gap-2">
				<div className="text-xs font-medium text-[var(--text)] uppercase tracking-wide">Recommended</div>
				{recommendedProviders.map(renderProviderRow)}
			</div>
			{otherProviders.length > 0 ? (
				<div className="flex flex-col gap-2">
					<button
						type="button"
						className="text-xs text-[var(--muted)] hover:text-[var(--text)] cursor-pointer bg-transparent border-none text-left flex items-center gap-1"
						onClick={() => setShowAllProviders((v) => !v)}
					>
						<span className={`inline-block transition-transform ${showOther ? "rotate-90" : ""}`}>{"\u25B6"}</span>
						All providers ({otherProviders.length} more)
					</button>
					{showOther ? otherProviders.map(renderProviderRow) : null}
				</div>
			) : null}
			{error && !configuring ? <ErrorPanel message={error} /> : null}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack || undefined}>
					{t("common:actions.back")}
				</button>
				<button
					type="button"
					className="provider-btn"
					onClick={onContinue}
					disabled={phase === "saving" || savingModels}
				>
					{t("common:actions.continue")}
				</button>
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
