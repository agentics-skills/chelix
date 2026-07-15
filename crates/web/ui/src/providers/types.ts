// ── Shared types for provider sub-modules ────────────────────

import type { ModelInfo, ProviderInfo } from "../types/model";

export type ModelEntry = ModelInfo;
export type { ProviderInfo };

export interface ProviderModalElements {
	modal: HTMLElement;
	body: HTMLElement;
	title: HTMLElement;
	close: HTMLElement;
}

export interface ValidationProgressState {
	progress: HTMLElement;
	progressBar: HTMLElement;
	progressText: HTMLElement;
	value: number;
}

export interface ValidationProgressUpdate {
	value: number;
	message: string;
}

export interface ValidationEventPayload {
	requestId?: string;
	phase?: string;
	message?: string;
	modelCount?: number;
	totalAttempts?: number;
	attempt?: number;
	modelId?: string;
}

export interface AddCustomPayload {
	providerName: string;
	displayName: string;
}

export interface ModelsData {
	models?: ModelEntry[];
}

export interface ProbeResult {
	error?: string;
	timeout?: boolean;
}

// Model selector wrapper with attached properties
export interface ModelSelectorWrapper extends HTMLElement {
	_errorArea?: HTMLElement;
	_resetSelection?: () => void;
	_renderModelsForBackend?: (backend: string) => void;
	_updateFilenameVisibility?: (backend: string) => void;
}
