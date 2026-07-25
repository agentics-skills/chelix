// ── Canonical model and provider wire contracts ─────────────

export type ModelModality = "text" | "image" | "audio" | "video" | "file";
export type ReasoningEffort = string;
export type ReasoningSummary = "auto" | "concise" | "detailed";
export type ReasoningInclude = "reasoning.encrypted_content";

export interface PartialReasoningMetadata {
	supported_efforts?: ReasoningEffort[];
	summary?: ReasoningSummary;
	include?: ReasoningInclude[];
}

export interface ModelReasoningMetadata {
	supported_efforts: ReasoningEffort[];
	summary?: ReasoningSummary;
	include: ReasoningInclude[];
}

export interface PartialModelMetadata {
	context_length?: number;
	max_input_tokens?: number;
	max_output_tokens?: number;
	input_modalities?: ModelModality[];
	output_modalities?: ModelModality[];
	tool_calling?: boolean;
	streaming?: boolean;
	zeroDataRetentionEnabled?: boolean;
	reasoning?: PartialReasoningMetadata;
}

export interface ModelMetadata {
	context_length: number;
	max_input_tokens: number;
	max_output_tokens: number;
	input_modalities: ModelModality[];
	output_modalities: ModelModality[];
	tool_calling: boolean;
	streaming: boolean;
	zeroDataRetentionEnabled: boolean;
	reasoning: ModelReasoningMetadata;
}

/** Ordered JSON object keyed by the provider's raw model ID. */
export type ModelConfigMap = Record<string, PartialModelMetadata>;

/** Full registry record returned by `models.list` and `models.list_all`. */
export interface ModelInfo extends ModelMetadata {
	id: string;
	provider: string;
	display_name: string;
	created_at: number | null;
	recommended: boolean;
	preferred: boolean;
	disabled: boolean;
	unsupported: boolean;
	unsupported_reason: string | null;
	unsupported_provider: string | null;
	unsupported_updated_at: number | null;
}

/** Provider row returned by `providers.available`. */
export interface ProviderInfo {
	name: string;
	displayName: string;
	authType: string;
	configured: boolean;
	defaultBaseUrl: string | null;
	baseUrl: string | null;
	models: ModelConfigMap;
	requiresModel: boolean;
	keyOptional: boolean;
	isCustom?: boolean;
	uiOrder: number;
}

export function rawModelId(modelId: string): string {
	const separator = modelId.lastIndexOf("::");
	return separator >= 0 ? modelId.slice(separator + 2) : modelId;
}

export function modelMetadataForConfig(model: ModelInfo): PartialModelMetadata {
	return {
		context_length: model.context_length,
		max_input_tokens: model.max_input_tokens,
		max_output_tokens: model.max_output_tokens,
		input_modalities: [...model.input_modalities],
		output_modalities: [...model.output_modalities],
		tool_calling: model.tool_calling,
		streaming: model.streaming,
		zeroDataRetentionEnabled: model.zeroDataRetentionEnabled,
		reasoning: {
			supported_efforts: [...model.reasoning.supported_efforts],
			summary: model.reasoning.summary,
			include: [...model.reasoning.include],
		},
	};
}

export function modelConfigMapFromSelection(
	models: readonly ModelInfo[],
	selectedModelIds: Iterable<string>,
): ModelConfigMap {
	const modelsById = new Map(models.map((model) => [model.id, model]));
	const result: ModelConfigMap = {};
	for (const modelId of selectedModelIds) {
		const model = modelsById.get(modelId);
		if (model) result[rawModelId(model.id)] = modelMetadataForConfig(model);
	}
	return result;
}

export function selectedModelIdsFromConfig(
	models: readonly ModelInfo[],
	configuredModels: ModelConfigMap,
): Set<string> {
	const modelIdsByRawId = new Map(models.map((model) => [rawModelId(model.id), model.id]));
	const selected = new Set<string>();
	for (const configuredModelId of Object.keys(configuredModels)) {
		const modelId = modelIdsByRawId.get(configuredModelId);
		if (modelId) selected.add(modelId);
	}
	return selected;
}
