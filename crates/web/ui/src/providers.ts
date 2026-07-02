// ── Provider modal — thin re-export barrel ──────────────────
//
// All implementation lives in ./providers/ sub-modules. This file
// re-exports the public API so existing import paths continue to work.

export { openModelSelectorForProvider, showApiKeyForm, showOAuthFlow } from "./providers/auth-flow";
export { showCustomProviderForm } from "./providers/custom-provider";
export { closeProviderModal, getProviderModal, openProviderModal } from "./providers/shared";
export type {
	AddCustomPayload,
	ModelEntry,
	ModelSelectorWrapper,
	ModelsData,
	ProbeResult,
	ProviderInfo,
	ProviderModalElements,
	ValidationEventPayload,
	ValidationProgressState,
	ValidationProgressUpdate,
} from "./providers/types";
