// ── Global sandbox status indicator ─────────────────────────

import { updateCommandInputUI, updateTokenBar } from "./chat-ui";
import * as S from "./state";

/** Synchronize read-only UI state from the canonical global sandbox mode. */
export function updateSandboxUI(): void {
	const mode = S.sandboxInfo?.mode;
	const label = S.sandboxLabel;
	const indicator = S.$("sandboxIndicator");

	if (!mode) {
		label?.replaceChildren();
		indicator?.classList.add("hidden");
		return;
	}

	const enabled = mode === "On";
	S.setSessionCommandMode(enabled ? "sandbox" : "host");
	S.setSessionCommandPromptSymbol(enabled || S.hostCommandIsRoot ? "#" : "$");
	updateCommandInputUI();
	updateTokenBar();

	indicator?.classList.remove("hidden");
	indicator?.setAttribute("data-mode", mode);
	indicator?.setAttribute("title", `Global sandbox mode: ${mode}`);
	if (label) label.textContent = `Sandbox ${mode}`;
}
