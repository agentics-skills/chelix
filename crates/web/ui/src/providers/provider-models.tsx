import type { VNode } from "preact";
import { useState } from "preact/hooks";
import { CheckboxField, SelectField, TextField } from "../components/forms/FormField";
import { sendRpc } from "../helpers";
import { t } from "../i18n";
import type { ModelInfo, ModelModality, ReasoningInclude, ReasoningSummary } from "../types/model";

const MODALITIES: ModelModality[] = ["text", "image", "audio", "video", "file"];

interface ProviderModelsProps {
	providerName: string;
	models: ModelInfo[];
	onChanged: () => void;
	editable: boolean;
	onToggleModel?: (model: ModelInfo) => void;
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
		["zeroDataRetentionEnabled", recordValue(model.zeroDataRetentionEnabled)],
		["reasoning_supported_efforts", JSON.stringify(model.reasoning_supported_efforts)],
		["reasoning_summary", recordValue(model.reasoning_summary)],
		["reasoning_include", model.reasoning_include === undefined ? "null" : JSON.stringify(model.reasoning_include)],
	];

	return (
		<dl className="mt-2 grid grid-cols-1 gap-x-4 gap-y-1 text-xs">
			{fields.map(([name, value]) => (
				<div key={name} className="flex min-w-0 gap-2">
					<dt className="min-w-0 break-all font-mono text-[var(--muted)]">{name}:</dt>
					<dd className="min-w-0 break-all text-[var(--text)]">{value}</dd>
				</div>
			))}
		</dl>
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

function ProviderModelRow({
	model,
	onToggle,
	onEdit,
	editDisabled,
}: {
	model: ModelInfo;
	onToggle: ((model: ModelInfo) => void) | null;
	onEdit: (() => void) | null;
	editDisabled: boolean;
}): VNode {
	return (
		<div className="flex flex-wrap items-start gap-3 py-1">
			<div className="min-w-0 w-full">
				<div className="flex items-center gap-2 min-w-0">
					<div className="text-sm font-medium text-[var(--text-strong)] truncate">{model.id}</div>
					<ProviderModelBadges model={model} />
				</div>
				<ModelRecord model={model} />
			</div>
			<div className="flex flex-wrap gap-2">
				{onToggle ? (
					<button
						type="button"
						className="provider-btn provider-btn-secondary provider-btn-sm"
						onClick={() => onToggle(model)}
					>
						{model.disabled ? t("common:actions.enable") : t("common:actions.disable")}
					</button>
				) : null}
				{onEdit ? (
					<button
						type="button"
						className="provider-btn provider-btn-secondary provider-btn-sm"
						disabled={editDisabled}
						onClick={onEdit}
					>
						{t("providers:editModel")}
					</button>
				) : null}
			</div>
		</div>
	);
}

interface ModelDraft {
	modelId: string;
	previousModelId: string | null;
	contextLength: string;
	maxInputTokens: string;
	maxOutputTokens: string;
	inputModalities: ModelModality[];
	outputModalities: ModelModality[];
	toolCalling: boolean;
	zeroDataRetentionEnabled: boolean;
	efforts: string;
	reasoningSummary: "" | ReasoningSummary;
	reasoningInclude: boolean;
}

function rawModelId(model: ModelInfo): string {
	const separator = model.id.lastIndexOf("::");
	return separator === -1 ? model.id : model.id.slice(separator + 2);
}

function emptyDraft(): ModelDraft {
	return {
		modelId: "",
		previousModelId: null,
		contextLength: "",
		maxInputTokens: "",
		maxOutputTokens: "",
		inputModalities: ["text"],
		outputModalities: ["text"],
		toolCalling: false,
		zeroDataRetentionEnabled: false,
		efforts: "",
		reasoningSummary: "",
		reasoningInclude: false,
	};
}

function draftFromModel(model: ModelInfo): ModelDraft {
	return {
		modelId: rawModelId(model),
		previousModelId: rawModelId(model),
		contextLength: String(model.context_length),
		maxInputTokens: String(model.max_input_tokens),
		maxOutputTokens: String(model.max_output_tokens),
		inputModalities: model.input_modalities,
		outputModalities: model.output_modalities,
		toolCalling: model.tool_calling,
		zeroDataRetentionEnabled: model.zeroDataRetentionEnabled,
		efforts: model.reasoning_supported_efforts.join(", "),
		reasoningSummary: model.reasoning_summary ?? "",
		reasoningInclude: model.reasoning_include?.includes("encrypted_content") ?? false,
	};
}

function toggleModality(current: ModelModality[], modality: ModelModality): ModelModality[] {
	return current.includes(modality) ? current.filter((item) => item !== modality) : current.concat(modality);
}

function ModelRecordForm({
	providerName,
	draft,
	setDraft,
	error,
	saving,
	onSubmit,
	onCancel,
	onDelete,
}: {
	providerName: string;
	draft: ModelDraft;
	setDraft: (draft: ModelDraft) => void;
	error: string;
	saving: boolean;
	onSubmit: () => void;
	onCancel: () => void;
	onDelete: (() => void) | null;
}): VNode {
	return (
		<form
			className="mt-3 flex flex-col gap-2 border-t border-[var(--border)] pt-3"
			onSubmit={(event) => {
				event.preventDefault();
				onSubmit();
			}}
		>
			<div className="text-sm font-medium text-[var(--text-strong)]">
				{t("providers:modelFormTitle")} — {providerName}
			</div>
			<TextField
				id={`${providerName}-model-id`}
				label={t("providers:modelId")}
				value={draft.modelId}
				onInput={(modelId) => setDraft({ ...draft, modelId })}
				required
			/>
			<TextField
				id={`${providerName}-context-length`}
				label={t("providers:contextLength")}
				value={draft.contextLength}
				onInput={(contextLength) => setDraft({ ...draft, contextLength })}
				type="number"
			/>
			<TextField
				id={`${providerName}-max-input`}
				label={t("providers:maxInputTokens")}
				value={draft.maxInputTokens}
				onInput={(maxInputTokens) => setDraft({ ...draft, maxInputTokens })}
				type="number"
			/>
			<TextField
				id={`${providerName}-max-output`}
				label={t("providers:maxOutputTokens")}
				value={draft.maxOutputTokens}
				onInput={(maxOutputTokens) => setDraft({ ...draft, maxOutputTokens })}
				type="number"
			/>
			<div className="text-xs text-[var(--muted)]">{t("providers:inputModalities")}</div>
			{MODALITIES.map((modality) => (
				<CheckboxField
					key={`${providerName}-in-${modality}`}
					id={`${providerName}-in-${modality}`}
					label={modality}
					checked={draft.inputModalities.includes(modality)}
					onChange={() => setDraft({ ...draft, inputModalities: toggleModality(draft.inputModalities, modality) })}
				/>
			))}
			<div className="text-xs text-[var(--muted)]">{t("providers:outputModalities")}</div>
			{MODALITIES.map((modality) => (
				<CheckboxField
					key={`${providerName}-out-${modality}`}
					id={`${providerName}-out-${modality}`}
					label={modality}
					checked={draft.outputModalities.includes(modality)}
					onChange={() => setDraft({ ...draft, outputModalities: toggleModality(draft.outputModalities, modality) })}
				/>
			))}
			<CheckboxField
				id={`${providerName}-tool-calling`}
				label={t("providers:toolCalling")}
				checked={draft.toolCalling}
				onChange={(toolCalling) => setDraft({ ...draft, toolCalling })}
			/>
			<CheckboxField
				id={`${providerName}-zdr`}
				label={t("providers:zeroDataRetention")}
				checked={draft.zeroDataRetentionEnabled}
				onChange={(zeroDataRetentionEnabled) => setDraft({ ...draft, zeroDataRetentionEnabled })}
			/>
			<TextField
				id={`${providerName}-efforts`}
				label={t("providers:reasoningEfforts")}
				value={draft.efforts}
				onInput={(efforts) => setDraft({ ...draft, efforts })}
			/>
			<SelectField
				id={`${providerName}-summary`}
				label={t("providers:reasoningSummary")}
				value={draft.reasoningSummary}
				onChange={(reasoningSummary) =>
					setDraft({ ...draft, reasoningSummary: reasoningSummary as ModelDraft["reasoningSummary"] })
				}
				options={[
					{ value: "", label: "" },
					{ value: "auto", label: "auto" },
					{ value: "concise", label: "concise" },
					{ value: "detailed", label: "detailed" },
				]}
			/>
			<CheckboxField
				id={`${providerName}-include`}
				label={`${t("providers:reasoningInclude")}: encrypted_content`}
				checked={draft.reasoningInclude}
				onChange={(reasoningInclude) => setDraft({ ...draft, reasoningInclude })}
			/>
			{error ? <div className="text-xs text-[var(--error)] whitespace-pre-line">{error}</div> : null}
			<div className="flex flex-wrap gap-2">
				<button type="submit" className="provider-btn provider-btn-sm" disabled={saving}>
					{t("providers:saveModel")}
				</button>
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm"
					onClick={onCancel}
					disabled={saving}
				>
					{t("providers:cancelModel")}
				</button>
				{onDelete ? (
					<button
						type="button"
						className="provider-btn provider-btn-danger provider-btn-sm"
						onClick={onDelete}
						disabled={saving}
					>
						{t("providers:deleteModel")}
					</button>
				) : null}
			</div>
		</form>
	);
}

export function ProviderModels({
	providerName,
	models,
	onChanged,
	editable,
	onToggleModel,
}: ProviderModelsProps): VNode {
	const [draft, setDraft] = useState<ModelDraft | null>(null);
	const [error, setError] = useState("");
	const [saving, setSaving] = useState(false);
	const [expanded, setExpanded] = useState(false);
	const hasMore = models.length > DEFAULT_VISIBLE_MODELS;
	const visibleModels = expanded || !hasMore ? models : models.slice(0, DEFAULT_VISIBLE_MODELS);

	async function saveDraft(): Promise<void> {
		if (!draft) return;
		setSaving(true);
		setError("");
		const efforts = draft.efforts.split(",").map((item) => item.trim());
		const metadata: Record<string, unknown> = {
			context_length: Number(draft.contextLength),
			max_input_tokens: Number(draft.maxInputTokens),
			max_output_tokens: Number(draft.maxOutputTokens),
			input_modalities: draft.inputModalities,
			output_modalities: draft.outputModalities,
			tool_calling: draft.toolCalling,
			zeroDataRetentionEnabled: draft.zeroDataRetentionEnabled,
			reasoning_supported_efforts: efforts,
		};
		if (draft.reasoningSummary) metadata.reasoning_summary = draft.reasoningSummary;
		if (draft.reasoningInclude) metadata.reasoning_include = ["encrypted_content"] satisfies ReasoningInclude[];
		const params: Record<string, unknown> = {
			provider: providerName,
			modelId: draft.modelId,
			metadata,
		};
		if (draft.previousModelId) params.previousModelId = draft.previousModelId;
		const response = await sendRpc("providers.upsert_model", params);
		setSaving(false);
		if (!response?.ok) {
			setError(response?.error?.message || t("providers:failedToUpdateModel"));
			return;
		}
		setDraft(null);
		onChanged();
	}

	async function deleteModel(modelId: string): Promise<void> {
		setSaving(true);
		setError("");
		const response = await sendRpc("providers.delete_model", { provider: providerName, modelId });
		setSaving(false);
		if (!response?.ok) {
			setError(response?.error?.message || t("providers:failedToUpdateModel"));
			return;
		}
		setDraft(null);
		onChanged();
	}

	function formNode(onDelete: (() => void) | null): VNode | null {
		if (!draft) return null;
		return (
			<ModelRecordForm
				providerName={providerName}
				draft={draft}
				setDraft={setDraft}
				error={error}
				saving={saving}
				onSubmit={() => {
					void saveDraft();
				}}
				onCancel={() => {
					setDraft(null);
					setError("");
				}}
				onDelete={onDelete}
			/>
		);
	}

	return (
		<div className="mt-2 flex flex-col gap-2">
			{editable ? (
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm self-start"
					disabled={saving}
					onClick={() => {
						setError("");
						setDraft(emptyDraft());
					}}
				>
					{t("providers:addModel")}
				</button>
			) : null}
			{editable && draft?.previousModelId === null ? formNode(null) : null}
			{models.length === 0 ? (
				<div className="text-xs text-[var(--muted)]">{t("providers:noActiveModels")}</div>
			) : (
				visibleModels.map((model) => (
					<div key={model.id}>
						<ProviderModelRow
							model={model}
							onToggle={onToggleModel ?? null}
							editDisabled={saving}
							onEdit={
								editable
									? () => {
											setError("");
											setDraft(draftFromModel(model));
										}
									: null
							}
						/>
						{editable && draft?.previousModelId === rawModelId(model)
							? formNode(() => {
									void deleteModel(draft.previousModelId || "");
								})
							: null}
					</div>
				))
			)}
			{hasMore ? (
				<button
					type="button"
					className="text-xs text-[var(--accent)] cursor-pointer bg-transparent border-none py-1 text-left hover:underline disabled:cursor-not-allowed disabled:no-underline disabled:opacity-50"
					disabled={expanded && draft !== null}
					onClick={() => setExpanded(!expanded)}
				>
					{expanded
						? t("providers:showFewerModels")
						: t("providers:showAllModels", { count: models.length - DEFAULT_VISIBLE_MODELS })}
				</button>
			) : null}
			{error && !draft ? <div className="text-xs text-[var(--error)] whitespace-pre-line">{error}</div> : null}
		</div>
	);
}
