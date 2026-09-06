// ── LLMs page (Preact + Signals) ──────────────────────────────

import { signal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useState } from "preact/hooks";
import { sendRpc } from "../helpers";
import { t } from "../i18n";
import { fetchModels } from "../models";
import { updateNavCount } from "../nav-counts";
import { openModelSelectorForProvider } from "../providers/auth-flow";
import { openProviderModal } from "../providers/shared";
import { connected } from "../signals";
import * as S from "../state";
import type { ModelInfo, ProviderInfo } from "../types/model";
import { ConfirmDialog, requestConfirm } from "../ui";

// ── Types ───────────────────────────────────────────────────

interface ProviderGroup {
	provider: string;
	providerDisplayName: string;
	models: ModelInfo[];
}

// ── Signals ─────────────────────────────────────────────────

const configuredModels = signal<ModelInfo[]>([]);
const providerMetaSig = signal<Map<string, ProviderInfo>>(new Map());
const loading = signal(false);
const deletingProvider = signal("");
const providerActionError = signal("");

function fetchProviders(): Promise<void> {
	loading.value = true;
	return Promise.all([sendRpc<ModelInfo[]>("models.list_all", {}), sendRpc<ProviderInfo[]>("providers.available", {})])
		.then(([modelsRes, providersRes]) => {
			loading.value = false;
			const providerMeta = new Map<string, ProviderInfo>();
			if (providersRes?.ok) {
				for (const provider of providersRes.payload || []) {
					if (provider.configured) providerMeta.set(provider.name, provider);
				}
			}
			providerMetaSig.value = providerMeta;

			configuredModels.value = modelsRes?.ok ? modelsRes.payload || [] : [];
			const providerNames = new Set([...providerMeta.keys(), ...configuredModels.value.map((model) => model.provider)]);
			updateNavCount("providers", providerNames.size);
		})
		.catch(() => {
			loading.value = false;
		});
}

function groupProviderRows(models: ModelInfo[], metaMap: Map<string, ProviderInfo>): ProviderGroup[] {
	const groups = new Map<string, ProviderGroup>();
	for (const provider of metaMap.values()) {
		groups.set(provider.name, {
			provider: provider.name,
			providerDisplayName: provider.displayName,
			models: [],
		});
	}

	for (const row of models) {
		const key = row.provider;
		if (!groups.has(key)) {
			const provider = metaMap.get(key);
			groups.set(key, {
				provider: key,
				providerDisplayName: provider?.displayName || key,
				models: [],
			});
		}
		groups.get(key)?.models.push(row);
	}

	const result = Array.from(groups.values());
	result.sort((a, b) => {
		const aOrder = metaMap?.get(a.provider)?.uiOrder;
		const bOrder = metaMap?.get(b.provider)?.uiOrder;
		const hasAOrder = typeof aOrder === "number" && Number.isFinite(aOrder);
		const hasBOrder = typeof bOrder === "number" && Number.isFinite(bOrder);
		if (hasAOrder && hasBOrder && aOrder !== bOrder) return aOrder - bOrder;
		if (hasAOrder && !hasBOrder) return -1;
		if (!hasAOrder && hasBOrder) return 1;
		return a.providerDisplayName.localeCompare(b.providerDisplayName);
	});
	return result;
}

const DEFAULT_VISIBLE_MODELS = 3;

function recordValue(value: string | number | boolean | null | undefined): string {
	return value === null || value === undefined ? "null" : String(value);
}

function ModelRecord({ model }: { model: ModelInfo }): VNode {
	const fields: Array<[string, string]> = [
		["id", model.id],
		["provider", model.provider],
		["preferred", recordValue(model.preferred)],
		["disabled", recordValue(model.disabled)],
		["context_length", recordValue(model.context_length)],
		["max_input_tokens", recordValue(model.max_input_tokens)],
		["max_output_tokens", recordValue(model.max_output_tokens)],
		["input_modalities", JSON.stringify(model.input_modalities)],
		["output_modalities", JSON.stringify(model.output_modalities)],
		["tool_calling", recordValue(model.tool_calling)],
		["streaming", recordValue(model.streaming)],
		["zeroDataRetentionEnabled", recordValue(model.zeroDataRetentionEnabled)],
		["reasoning_supported_efforts", JSON.stringify(model.reasoning_supported_efforts)],
		["reasoning_summary", recordValue(model.reasoning_summary)],
		["reasoning_include", model.reasoning_include === undefined ? "null" : JSON.stringify(model.reasoning_include)],
	];

	return (
		<dl className="mt-2 grid grid-cols-1 gap-x-4 gap-y-1 text-xs sm:grid-cols-2">
			{fields.map(([name, value]) => (
				<div key={name} className="flex min-w-0 gap-2">
					<dt className="shrink-0 font-mono text-[var(--muted)]">{name}:</dt>
					<dd className="min-w-0 break-all text-[var(--text)]">{value}</dd>
				</div>
			))}
		</dl>
	);
}

interface ProviderActionsProps {
	hasModels: boolean;
	isDeleting: boolean;
	onSelectModels: () => void;
	onDelete: () => void;
}

function ProviderActions({ hasModels, isDeleting, onSelectModels, onDelete }: ProviderActionsProps): VNode {
	return (
		<div className="flex gap-2 shrink-0">
			{hasModels ? (
				<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={onSelectModels}>
					{t("providers:preferredModels.button")}
				</button>
			) : null}
			<button
				type="button"
				className="provider-btn provider-btn-danger provider-btn-sm"
				disabled={isDeleting}
				onClick={onDelete}
			>
				{isDeleting ? t("common:status.deleting") : t("common:actions.delete")}
			</button>
		</div>
	);
}

function ProviderModelBadges({ model }: { model: ModelInfo }): VNode {
	return (
		<>
			{model.preferred ? <span className="recommended-badge">{t("providers:preferred")}</span> : null}
			{model.tool_calling ? null : <span className="provider-item-badge warning">{t("providers:chatOnly")}</span>}
			{model.disabled ? <span className="provider-item-badge muted">{t("providers:disabled")}</span> : null}
		</>
	);
}

interface ProviderModelRowProps {
	model: ModelInfo;
	onToggle: (model: ModelInfo) => void;
}

function ProviderModelRow({ model, onToggle }: ProviderModelRowProps): VNode {
	return (
		<div className="flex items-start justify-between gap-3 py-1">
			<div className="min-w-0 flex-1">
				<div className="flex items-center gap-2 min-w-0">
					<div className="text-sm font-medium text-[var(--text-strong)] truncate">{model.id}</div>
					<ProviderModelBadges model={model} />
				</div>
				<ModelRecord model={model} />
			</div>
			<button
				type="button"
				className="provider-btn provider-btn-secondary provider-btn-sm"
				onClick={() => onToggle(model)}
			>
				{model.disabled ? t("common:actions.enable") : t("common:actions.disable")}
			</button>
		</div>
	);
}

interface ProviderModelListProps {
	models: ModelInfo[];
	hasMore: boolean;
	expanded: boolean;
	hiddenCount: number;
	onToggleModel: (model: ModelInfo) => void;
	onToggleExpanded: () => void;
}

function ProviderModelList({
	models,
	hasMore,
	expanded,
	hiddenCount,
	onToggleModel,
	onToggleExpanded,
}: ProviderModelListProps): VNode {
	if (models.length === 0) {
		return <div className="mt-2 text-xs text-[var(--muted)]">{t("providers:noActiveModels")}</div>;
	}
	return (
		<div className="mt-2 flex flex-col gap-2">
			{models.map((model) => (
				<ProviderModelRow key={model.id} model={model} onToggle={onToggleModel} />
			))}
			{hasMore ? (
				<button
					type="button"
					className="text-xs text-[var(--accent)] cursor-pointer bg-transparent border-none py-1 text-left hover:underline"
					onClick={onToggleExpanded}
				>
					{expanded ? t("providers:showFewerModels") : t("providers:showAllModels", { count: hiddenCount })}
				</button>
			) : null}
		</div>
	);
}

function ProviderSection({ group }: { group: ProviderGroup }): VNode {
	const [expanded, setExpanded] = useState(false);
	const hasMore = group.models.length > DEFAULT_VISIBLE_MODELS;
	const visibleModels = expanded || !hasMore ? group.models : group.models.slice(0, DEFAULT_VISIBLE_MODELS);
	const hiddenCount = group.models.length - DEFAULT_VISIBLE_MODELS;

	function onDeleteProvider(): void {
		if (deletingProvider.value) return;
		requestConfirm(t("providers:removeProviderConfirm", { name: group.providerDisplayName })).then((yes) => {
			if (!yes) return;
			deletingProvider.value = group.provider;
			providerActionError.value = "";
			sendRpc("providers.remove_key", { provider: group.provider })
				.then((res) => {
					if (res?.ok) {
						configuredModels.value = configuredModels.value.filter((entry) => entry.provider !== group.provider);
						fetchModels();
						fetchProviders();
						return;
					}
					providerActionError.value = res?.error?.message || t("providers:failedToDeleteProvider");
				})
				.catch(() => {
					providerActionError.value = t("providers:failedToDeleteProvider");
				})
				.finally(() => {
					deletingProvider.value = "";
				});
		});
	}

	function onToggleModel(model: ModelInfo): void {
		const method = model.disabled ? "models.enable" : "models.disable";
		sendRpc(method, { modelId: model.id }).then((res) => {
			if (res?.ok) {
				providerActionError.value = "";
				configuredModels.value = configuredModels.value.map((entry) =>
					entry.id === model.id ? { ...entry, disabled: !model.disabled } : entry,
				);
				fetchModels();
				fetchProviders();
			} else {
				providerActionError.value = res?.error?.message || t("providers:failedToUpdateModel");
			}
		});
	}

	function onSelectModels(): void {
		openModelSelectorForProvider(group.provider, group.providerDisplayName);
	}

	const isDeleting = deletingProvider.value === group.provider;

	return (
		<div id={`provider-${group.provider}`} className="max-w-form py-1">
			<div className="flex items-center justify-between gap-3">
				<div className="flex items-center gap-2 min-w-0">
					<h3 className="text-base font-semibold text-[var(--text-strong)] truncate">{group.providerDisplayName}</h3>
				</div>
				<ProviderActions
					hasModels={group.models.length > 0}
					isDeleting={isDeleting}
					onSelectModels={onSelectModels}
					onDelete={onDeleteProvider}
				/>
			</div>
			<div className="mt-2 border-b border-[var(--border)]" />
			<ProviderModelList
				models={visibleModels}
				hasMore={hasMore}
				expanded={expanded}
				hiddenCount={hiddenCount}
				onToggleModel={onToggleModel}
				onToggleExpanded={() => setExpanded(!expanded)}
			/>
		</div>
	);
}

function ProvidersPageComponent(): VNode {
	useEffect(() => {
		if (connected.value) fetchProviders();
	}, [connected.value]);

	S.setRefreshProvidersPage(fetchProviders);

	return (
		<>
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<div className="flex items-center gap-3">
					<h2 id="providersTitle" className="text-lg font-medium text-[var(--text-strong)]">
						{t("providers:title")}
					</h2>
					<button
						type="button"
						id="providersAddLlmBtn"
						className="provider-btn"
						onClick={() => {
							if (connected.value) openProviderModal();
						}}
					>
						{t("providers:addLlm")}
					</button>
				</div>
				<p className="text-xs text-[var(--muted)] leading-relaxed max-w-form" style={{ margin: 0 }}>
					{t("providers:description")}
				</p>
				{providerActionError.value ? (
					<div className="text-xs text-[var(--danger,#ef4444)] max-w-form">{providerActionError.value}</div>
				) : null}

				{(() => {
					const groups = groupProviderRows(configuredModels.value, providerMetaSig.value);
					if (loading.value && configuredModels.value.length === 0) {
						return (
							<div id="providersLoadingState" className="text-xs text-[var(--muted)]">
								{t("common:status.loading")}
							</div>
						);
					}
					if (configuredModels.value.length === 0) {
						return (
							<div id="providersEmptyState" className="text-xs text-[var(--muted)]" style={{ padding: "12px 0" }}>
								{t("providers:noProvidersConfigured")}
							</div>
						);
					}
					return (
						<div id="providersConfiguredList" style={{ maxWidth: "600px" }}>
							{groups.length > 1 ? (
								<div className="flex flex-wrap gap-1 mb-3">
									{groups.map((g) => (
										<button
											type="button"
											key={g.provider}
											className="text-xs px-2 py-1 rounded-md border border-[var(--border)] bg-[var(--surface)] text-[var(--muted)] hover:text-[var(--text)] hover:border-[var(--border-strong)] cursor-pointer"
											onClick={() => {
												const el = document.getElementById(`provider-${g.provider}`);
												if (el)
													el.scrollIntoView({
														behavior: "smooth",
														block: "start",
													});
											}}
										>
											{g.providerDisplayName}
											<span className="ml-1 opacity-60">{g.models.length}</span>
										</button>
									))}
								</div>
							) : null}
							<div
								style={{
									display: "flex",
									flexDirection: "column",
									gap: "6px",
									marginBottom: "12px",
								}}
							>
								{groups.map((g) => (
									<ProviderSection key={g.provider} group={g} />
								))}
							</div>
						</div>
					);
				})()}
			</div>
			<ConfirmDialog />
		</>
	);
}

let _providersContainer: HTMLElement | null = null;

export function initProviders(container: HTMLElement): void {
	_providersContainer = container;
	container.style.cssText = "flex-direction:column;padding:0;overflow:hidden;";
	render(<ProvidersPageComponent />, container);
}

export function teardownProviders(): void {
	S.setRefreshProvidersPage(null);
	if (_providersContainer) render(null, _providersContainer);
	_providersContainer = null;
}
