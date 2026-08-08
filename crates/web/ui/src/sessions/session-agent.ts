// ── Session agent and model settings ────────────────────────────

import { sendRpc } from "../helpers";
import { modelDisplayLabel, modelTitle } from "../models";
import { restoreReasoningEffort } from "../reasoning-toggle";
import * as S from "../state";
import { modelStore } from "../stores/model-store";
import { sessionStore } from "../stores/session-store";
import type { RpcResponse } from "../types/rpc";
import type { SetSessionAgentPayload } from "../types/session";

interface SessionModelSettings {
	model?: string | null;
	reasoningEffort?: string | null;
}

export function restoreSessionModelSettings(settings: SessionModelSettings): void {
	if (settings.model) {
		modelStore.select(settings.model);
		localStorage.setItem("chelix-model", settings.model);
		const found = modelStore.getById(settings.model);
		if (S.modelComboLabel) {
			const label = found ? modelDisplayLabel(found) : settings.model;
			S.modelComboLabel.textContent = label;
			S.modelComboLabel.title = found ? modelTitle(found) : label;
		}
	}
	restoreReasoningEffort(settings.reasoningEffort);
}

function isSetSessionAgentPayload(payload: unknown): payload is SetSessionAgentPayload {
	if (!payload || typeof payload !== "object") return false;
	const value = payload as Partial<SetSessionAgentPayload>;
	return (
		value.ok === true &&
		typeof value.agent_id === "string" &&
		value.agent_id.length > 0 &&
		(value.model === null || typeof value.model === "string") &&
		(value.reasoningEffort === null || typeof value.reasoningEffort === "string") &&
		Number.isInteger(value.version) &&
		(value.version as number) >= 0
	);
}

export function setSessionAgent(sessionKey: string, agentId: string): Promise<RpcResponse<SetSessionAgentPayload>> {
	return sendRpc<SetSessionAgentPayload>("agents.set_session", {
		session_key: sessionKey,
		agent_id: agentId,
	}).then((res) => {
		if (!res?.ok) return res;
		if (!isSetSessionAgentPayload(res.payload)) {
			return {
				ok: false,
				error: {
					code: "INVALID_RESPONSE",
					message: "Agent switch returned invalid session model settings",
				},
			};
		}

		const payload = res.payload;
		const session = sessionStore.getByKey(sessionKey);
		if (session) {
			session.agent_id = payload.agent_id;
			session.model = payload.model || "";
			session.reasoningEffort = payload.reasoningEffort ?? "";
			session.version = payload.version;
			session.dataVersion.value++;
		}
		if (sessionStore.activeSessionKey.value === sessionKey) {
			restoreSessionModelSettings(payload);
		}
		return res;
	});
}
