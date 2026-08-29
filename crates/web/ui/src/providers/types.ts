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
