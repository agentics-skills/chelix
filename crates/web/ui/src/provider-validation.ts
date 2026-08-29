import { sendRpc } from "./helpers";
import type { RpcResponse } from "./types/rpc";

const COMPLETION_ENDPOINT_SUFFIXES = ["/chat/completions", "/responses"];

export function providerBaseUrlError(baseUrl: string | null | undefined): string | null {
	const trimmed = baseUrl?.trim().replace(/\/+$/, "") || "";
	if (!trimmed) return null;
	try {
		const parsed = new URL(trimmed);
		if (!((parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.hostname)) {
			return "Endpoint URL must include an http:// or https:// scheme and a host.";
		}
	} catch {
		return "Endpoint URL must be a valid HTTP(S) URL, such as 'https://api.example.com/v1'.";
	}
	const lower = trimmed.toLowerCase();
	const suffix = COMPLETION_ENDPOINT_SUFFIXES.find((value) => lower.endsWith(value));
	if (!suffix) return null;
	const suggested = trimmed.slice(0, -suffix.length) || trimmed;
	return `Endpoint URL should be the API base URL, not the completion path. Use '${suggested}' instead of '${trimmed}'.`;
}

/** Build the payload for a `providers.save_key` RPC call. */
export function buildSaveKeyPayload(
	providerName: string,
	apiKey: string,
	baseUrl: string | null,
): Record<string, string> {
	const payload: Record<string, string> = { provider: providerName, apiKey };
	if (baseUrl) payload.baseUrl = baseUrl;
	return payload;
}

/** Persist provider credentials via the `providers.save_key` RPC. */
export function saveProviderKey(providerName: string, apiKey: string, baseUrl: string | null): Promise<RpcResponse> {
	const payload = buildSaveKeyPayload(providerName, apiKey, baseUrl);
	return sendRpc("providers.save_key", payload);
}
