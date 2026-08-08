import { onEvent } from "./events";
import type { ValidationEventPayload, ValidationProgressUpdate } from "./providers/types";

export const VALIDATION_HINT_TEXT = "";
export const VALIDATION_HINT_RUNNING_TEXT = "Discovering models...";

const VALIDATION_PROGRESS_EVENT = "providers.validate.progress";
const STATIC_PHASE_UPDATES: Record<string, ValidationProgressUpdate> = {
	start: { value: 8, message: "Starting provider validation..." },
	probe_succeeded: { value: 94, message: "Model probe succeeded." },
	complete: { value: 100, message: "Validation complete." },
	error: { value: 98, message: "Validation failed." },
};
const PROBE_PHASES = new Set(["probe_started", "probe_failed", "probe_timeout"]);

function normalizeAttempt(value: number | undefined, fallback: number): number {
	if (!Number.isFinite(value)) return fallback;
	return Math.max(1, Math.floor(value as number));
}

function stripModelNamespace(modelId: string | undefined): string | undefined {
	if (!modelId) return modelId;
	const separator = modelId.lastIndexOf("::");
	return separator >= 0 ? modelId.slice(separator + 2) : modelId;
}

function discoveredCandidatesUpdate(payload: ValidationEventPayload): ValidationProgressUpdate {
	const count = Number.isFinite(payload.modelCount) ? payload.modelCount : null;
	return {
		value: 24,
		message:
			payload.message || (count == null ? "Discovered candidate models." : `Discovered ${count} candidate models.`),
	};
}

function probeProgressUpdate(payload: ValidationEventPayload): ValidationProgressUpdate {
	const total = normalizeAttempt(payload.totalAttempts, 1);
	const attempt = Math.min(normalizeAttempt(payload.attempt, 1), total);
	const modelName = stripModelNamespace(payload.modelId);
	const defaultMessage = modelName
		? `Probing ${modelName} (${attempt}/${total})...`
		: `Probing model ${attempt}/${total}...`;
	return {
		value: 24 + (attempt / total) * 62,
		message: payload.message || defaultMessage,
	};
}

function staticPhaseUpdate(payload: ValidationEventPayload): ValidationProgressUpdate | null {
	const update = payload.phase ? STATIC_PHASE_UPDATES[payload.phase] : null;
	if (!update) return null;
	return { ...update, message: payload.message || update.message };
}

function progressFromValidationEvent(payload: ValidationEventPayload): ValidationProgressUpdate | null {
	if (!payload.phase) return null;
	if (payload.phase === "candidates_discovered") return discoveredCandidatesUpdate(payload);
	if (PROBE_PHASES.has(payload.phase)) return probeProgressUpdate(payload);
	return staticPhaseUpdate(payload);
}

export function clampValidationProgressPercent(value: number): number {
	if (!Number.isFinite(value)) return 0;
	return Math.max(0, Math.min(100, value));
}

export function createValidationRequestId(): string {
	const nonce = Math.random().toString(36).slice(2, 10);
	return `validate-${Date.now()}-${nonce}`;
}

export function subscribeValidationProgress(
	requestId: string,
	onProgress: (update: ValidationProgressUpdate, payload: ValidationEventPayload) => void,
): () => void {
	if (!requestId) return () => undefined;
	const off = onEvent(VALIDATION_PROGRESS_EVENT, (rawPayload: unknown) => {
		const payload = rawPayload as ValidationEventPayload;
		if (!payload || payload.requestId !== requestId) return;
		const update = progressFromValidationEvent(payload);
		if (update) onProgress(update, payload);
	});
	return off;
}
