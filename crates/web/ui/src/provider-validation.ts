import { sendRpc } from "./helpers";
import type { RpcResponse } from "./types/rpc";

const MODEL_SERVICE_NOT_CONFIGURED = "model service not configured";
const MODEL_TEST_RETRY_ATTEMPTS = 40;
const MODEL_TEST_RETRY_DELAY_MS = 250;
const COMPLETION_ENDPOINT_SUFFIXES = ["/chat/completions", "/responses"];

export interface TestModelResult {
	ok: boolean;
	error?: string;
}

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

function includesAny(value: string, candidates: readonly string[]): boolean {
	return candidates.some((candidate) => value.includes(candidate));
}

/** Map raw error strings to user-friendly messages. */
export function humanizeProbeError(error: string | null | undefined): string | null | undefined {
	if (!error || typeof error !== "string") return error;
	const lower = error.toLowerCase();

	if (includesAny(lower, ["401", "unauthorized", "invalid api key", "invalid x-api-key"])) {
		return "Invalid API key. Please double-check and try again.";
	}
	if (includesAny(lower, ["403", "forbidden"])) {
		return "Your API key doesn't have access. Check your account permissions.";
	}
	if (lower.includes("permission")) return error;
	if (includesAny(lower, ["429", "rate limit", "too many requests"])) {
		return "Rate limited by the provider. Wait a moment and try again.";
	}
	if (includesAny(lower, ["timeout", "timed out"])) {
		return "Connection timed out. Check your endpoint URL and try again.";
	}
	if (includesAny(lower, ["connection refused", "econnrefused"])) {
		return "Connection refused. Make sure the provider endpoint is running and reachable.";
	}
	if (includesAny(lower, ["dns", "getaddrinfo", "name or service not known"])) {
		return "Could not resolve the endpoint address. Check the URL and try again.";
	}
	if (includesAny(lower, ["404", "not found"])) {
		return "Model not found at this endpoint. Make sure it is configured and try again.";
	}

	return error;
}

export function isModelServiceNotConfigured(error: string): boolean {
	if (!error || typeof error !== "string") return false;
	return error.toLowerCase().includes(MODEL_SERVICE_NOT_CONFIGURED);
}

export function isTimeoutError(error: string): boolean {
	if (!error || typeof error !== "string") return false;
	const lower = error.toLowerCase();
	return lower.includes("timeout") || lower.includes("timed out");
}

/** Test a single model from the live registry. */
export async function testModel(modelId: string): Promise<TestModelResult> {
	for (let attempt = 0; attempt < MODEL_TEST_RETRY_ATTEMPTS; attempt++) {
		const res: RpcResponse = await sendRpc("models.test", { modelId });
		if (res?.ok) return { ok: true };

		const message = res?.error?.message || "Model test failed.";
		const lower = String(message).toLowerCase();
		const shouldRetry = lower.includes(MODEL_SERVICE_NOT_CONFIGURED) && attempt < MODEL_TEST_RETRY_ATTEMPTS - 1;
		if (!shouldRetry) {
			return {
				ok: false,
				error: humanizeProbeError(message) as string,
			};
		}

		await new Promise<void>((resolve) => {
			window.setTimeout(resolve, MODEL_TEST_RETRY_DELAY_MS);
		});
	}

	return {
		ok: false,
		error: humanizeProbeError("Model test failed.") as string,
	};
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
