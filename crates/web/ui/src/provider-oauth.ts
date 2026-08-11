import { sendRpc } from "./helpers";
import type { RpcResponse } from "./types/rpc";

interface OAuthStartPayload {
	alreadyAuthenticated?: boolean;
	authUrl?: string;
}

export interface OAuthStartResult {
	status: "already" | "browser" | "error";
	authUrl?: string;
	error?: string;
}

function normalizeOAuthStartResponse(res: RpcResponse<OAuthStartPayload> | null): OAuthStartResult {
	const payload = res?.payload;

	if (res?.ok && payload?.alreadyAuthenticated) {
		return {
			status: "already",
		};
	}

	if (res?.ok && payload?.authUrl) {
		return {
			status: "browser",
			authUrl: payload.authUrl,
		};
	}

	return {
		status: "error",
		error: res?.error?.message || "Failed to start OAuth",
	};
}

export function startProviderOAuth(providerName: string): Promise<OAuthStartResult> {
	return sendRpc("providers.oauth.start", {
		provider: providerName,
		redirectUri: `${window.location.origin}/auth/callback`,
	}).then((res) => normalizeOAuthStartResponse(res as RpcResponse<OAuthStartPayload>));
}

export function completeProviderOAuth(providerName: string, callback: string): Promise<RpcResponse> {
	return sendRpc("providers.oauth.complete", {
		provider: providerName,
		callback,
	});
}
