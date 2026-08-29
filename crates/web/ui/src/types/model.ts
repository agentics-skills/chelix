// ── Canonical model metadata contracts ───────────────────────

export type ReasoningSummary = "auto" | "concise" | "detailed";
export type ReasoningInclude = "encrypted_content";
export type ModelModality = "text" | "image" | "audio" | "video" | "file";

export interface ModelMetadata {
	context_length: number;
	max_input_tokens: number;
	max_output_tokens: number;
	input_modalities: ModelModality[];
	output_modalities: ModelModality[];
	tool_calling: boolean;
	streaming: boolean;
	zeroDataRetentionEnabled: boolean;
	reasoning_supported_efforts: string[];
	reasoning_summary?: ReasoningSummary;
	reasoning_include?: ReasoningInclude[];
}

export interface ModelInfo extends ModelMetadata {
	id: string;
	provider: string;
	preferred?: boolean;
	disabled?: boolean;
}

export interface ProviderInfo {
	name: string;
	displayName: string;
	configured: boolean;
	defaultBaseUrl?: string | null;
	baseUrl?: string | null;
	requiresModel: boolean;
	keyOptional: boolean;
	isCustom?: boolean;
	uiOrder?: number;
}
