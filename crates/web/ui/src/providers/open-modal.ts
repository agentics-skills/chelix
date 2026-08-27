// ── Open provider modal implementation ───────────────────────
//
// Separated to break circular dependency: shared.ts defines the
// openProviderModal stub that dynamically imports this module,
// and this module imports from the sub-modules that depend on shared.ts.

import { sendRpc } from "../helpers";
import type { RpcResponse } from "../types/rpc";
import { showApiKeyForm } from "./auth-flow";
import { showOpenAiCompatibleForm } from "./openai-compatible";
import { els } from "./shared";
import type { ProviderInfo } from "./types";

export function openProviderModalImpl(): void {
	const m = els();
	m.modal.classList.remove("hidden");
	m.title.textContent = "Add LLM";
	m.body.textContent = "Loading...";
	sendRpc<ProviderInfo[]>("providers.available", {}).then((res: RpcResponse<ProviderInfo[]>) => {
		if (!res?.ok) {
			m.body.textContent = "Failed to load LLM providers.";
			return;
		}
		const providers: ProviderInfo[] = (res.payload as ProviderInfo[]) || [];

		providers.sort((a: ProviderInfo, b: ProviderInfo) => {
			const aOrder = typeof a.uiOrder === "number" && Number.isFinite(a.uiOrder) ? a.uiOrder : Number.MAX_SAFE_INTEGER;
			const bOrder = typeof b.uiOrder === "number" && Number.isFinite(b.uiOrder) ? b.uiOrder : Number.MAX_SAFE_INTEGER;
			if (aOrder !== bOrder) return aOrder - bOrder;
			return a.displayName.localeCompare(b.displayName);
		});

		m.body.textContent = "";
		providers.forEach((p: ProviderInfo) => {
			const item = document.createElement("div");
			// Don't gray out configured providers - users can add multiple
			item.className = "provider-item";
			const name = document.createElement("span");
			name.className = "provider-item-name";
			name.textContent = p.displayName;
			item.appendChild(name);

			const badges = document.createElement("div");
			badges.className = "badge-row";

			if (p.configured) {
				const check = document.createElement("span");
				check.className = "provider-item-badge configured";
				check.textContent = "configured";
				badges.appendChild(check);
			}

			if (p.isCustom) {
				const customBadge = document.createElement("span");
				customBadge.className = "provider-item-badge api-key";
				customBadge.textContent = "Custom";
				badges.appendChild(customBadge);
			}
			item.appendChild(badges);

			item.addEventListener("click", () => showApiKeyForm(p));
			m.body.appendChild(item);
		});

		const separator = document.createElement("div");
		separator.className = "border-t border-[var(--border)] my-2";
		m.body.appendChild(separator);

		const compatibleItem = document.createElement("div");
		compatibleItem.className = "provider-item";

		const compatibleName = document.createElement("span");
		compatibleName.className = "provider-item-name";
		compatibleName.textContent = "OpenAI Compatible";
		compatibleItem.appendChild(compatibleName);

		const compatibleBadges = document.createElement("div");
		compatibleBadges.className = "badge-row";
		const endpointBadge = document.createElement("span");
		endpointBadge.className = "provider-item-badge api-key";
		endpointBadge.textContent = "Any Endpoint";
		compatibleBadges.appendChild(endpointBadge);
		compatibleItem.appendChild(compatibleBadges);

		compatibleItem.addEventListener("click", () => showOpenAiCompatibleForm(providers));
		m.body.appendChild(compatibleItem);
	});
}
