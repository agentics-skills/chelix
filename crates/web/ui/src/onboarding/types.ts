// ── Shared types for onboarding sub-modules ──────────────────

export type { ModelInfo as ModelSelectorRow, ModelInfo as RawModelRow, ProviderInfo } from "../types/model";

export interface ValidationResult {
	ok: boolean;
	message: string | null;
}

export interface OAuthInfo {
	status: string;
	uri?: string;
	code?: string;
}

export interface IdentityInfo {
	user_name?: string;
	name?: string;
	emoji?: string;
	theme?: string;
	[key: string]: unknown;
}

export interface KeyHelp {
	text: string;
	url?: string;
	label?: string;
}

export interface ProbeResult {
	error?: string;
}
