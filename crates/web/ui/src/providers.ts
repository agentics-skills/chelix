// ── Provider modal — thin re-export barrel ──────────────────
//
// All implementation lives in ./providers/ sub-modules.

export { openModelSelectorForProvider, showApiKeyForm, showOAuthFlow } from "./providers/auth-flow";
export { showCustomProviderForm } from "./providers/custom-provider";
export { closeProviderModal, openProviderModal } from "./providers/shared";
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
