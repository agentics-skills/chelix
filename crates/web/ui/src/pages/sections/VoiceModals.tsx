// ── Voice modals — extracted from VoiceSection ──────────────

import { signal } from "@preact/signals";
import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import * as gon from "../../gon";
import { sendRpc } from "../../helpers";
import { fetchPhrase } from "../../tts-phrases";
import { targetValue } from "../../typed-events";
import { Modal } from "../../ui";
import {
	createVoicePersona,
	decodeBase64Safe,
	saveVoiceKey,
	saveVoiceSettings,
	testTtsWithPersona,
	updateVoicePersona,
	type VoicePersonaPrompt,
	type VoicePersonaProviderBinding,
	type VoicePersonaResponse,
} from "../../voice-utils";
import type { RpcResponse } from "./_shared";
import { rerender } from "./_shared";

function cloneHidden(id: string): HTMLElement | null {
	const el = document.getElementById(id);
	if (!el) return null;
	const clone = el.cloneNode(true) as HTMLElement;
	clone.removeAttribute("id");
	clone.style.display = "";
	return clone;
}

// ── Shared signals ──────────────────────────────────────────

export const voiceShowAddModal = signal(false);
export const voiceSelectedProvider = signal<string | null>(null);
export const voiceSelectedProviderData = signal<VoiceProviderData | null>(null);

// ── Shared interfaces ───────────────────────────────────────

export interface VoiceProviderData {
	id: string;
	name: string;
	description?: string;
	type?: string;
	category?: string;
	available?: boolean;
	enabled?: boolean;
	preferred?: boolean;
	keySource?: string;
	settingsSummary?: string;
	binaryPath?: string;
	statusMessage?: string;
	keyPlaceholder?: string;
	keyUrl?: string;
	keyUrlLabel?: string;
	hint?: string;
	capabilities?: {
		baseUrl?: boolean;
		customModel?: boolean;
		modelChoices?: string[];
		realtimeModelChoices?: string[];
	};
	settings?: { baseUrl?: string; voiceId?: string; voice?: string; model?: string; languageCode?: string };
}

export interface VoiceTesting {
	id: string;
	type: string;
	phase: string;
}

export interface VoiceTestResult {
	text?: string | null;
	success?: boolean;
	error?: string | null;
}

export interface VoxtralRequirements {
	os?: string;
	arch?: string;
	compatible?: boolean;
	reasons?: string[];
	python?: { available?: boolean; version?: string };
	cuda?: { available?: boolean; gpu_name?: string; memory_mb?: number };
}

// ── PersonaEditModal ────────────────────────────────────────

interface PersonaEditModalProps {
	editingId: string;
	existingPersona: VoicePersonaResponse | null;
	onClose: () => void;
	onSaved: () => void;
}

interface PersonaDraft {
	id: string;
	label: string;
	description: string;
	profile: string;
	style: string;
	accent: string;
	pacing: string;
	openaiVoice: string;
	openaiModel: string;
	elevenVoice: string;
}

type PersonaDraftField = keyof PersonaDraft;
type PersonaDraftChange = (field: PersonaDraftField, value: string) => void;

function personaBinding(
	persona: VoicePersonaResponse | null,
	provider: string,
): VoicePersonaProviderBinding | undefined {
	return persona?.persona.provider_bindings?.find((binding) => binding.provider === provider);
}

function initialPersonaDraft(persona: VoicePersonaResponse | null): PersonaDraft {
	return {
		id: persona?.persona.id ?? "",
		label: persona?.persona.label ?? "",
		description: persona?.persona.description ?? "",
		profile: persona?.persona.prompt.profile ?? "",
		style: persona?.persona.prompt.style ?? "",
		accent: persona?.persona.prompt.accent ?? "",
		pacing: persona?.persona.prompt.pacing ?? "",
		openaiVoice: personaBinding(persona, "openai")?.voice_id ?? "",
		openaiModel: personaBinding(persona, "openai")?.model ?? "gpt-4o-mini-tts",
		elevenVoice: personaBinding(persona, "elevenlabs")?.voice_id ?? "",
	};
}

function personaPrompt(draft: PersonaDraft): VoicePersonaPrompt {
	const prompt: VoicePersonaPrompt = {};
	if (draft.profile) prompt.profile = draft.profile;
	if (draft.style) prompt.style = draft.style;
	if (draft.accent) prompt.accent = draft.accent;
	if (draft.pacing) prompt.pacing = draft.pacing;
	return prompt;
}

function personaBindings(draft: PersonaDraft): VoicePersonaProviderBinding[] {
	const bindings: VoicePersonaProviderBinding[] = [];
	if (draft.openaiVoice || draft.openaiModel) {
		bindings.push({
			provider: "openai",
			voice_id: draft.openaiVoice || undefined,
			model: draft.openaiModel || undefined,
		});
	}
	if (draft.elevenVoice) bindings.push({ provider: "elevenlabs", voice_id: draft.elevenVoice });
	return bindings;
}

function personaValidationError(isNew: boolean, draft: PersonaDraft): string | null {
	return isNew && !(draft.id && draft.label) ? "ID and Label are required." : null;
}

async function persistPersona(isNew: boolean, editingId: string, draft: PersonaDraft): Promise<void> {
	const prompt = personaPrompt(draft);
	const providerBindings = personaBindings(draft);
	if (isNew) {
		await createVoicePersona({
			id: draft.id,
			label: draft.label,
			description: draft.description || undefined,
			prompt,
			providerBindings,
		});
		return;
	}
	await updateVoicePersona(editingId, {
		label: draft.label || undefined,
		description: draft.description || undefined,
		prompt,
		providerBindings,
	});
}

function personaTestInstructions(draft: PersonaDraft): string {
	const prompt = personaPrompt(draft);
	return [
		`Persona: ${draft.label || "Test"}`,
		prompt.profile ? `Profile: ${prompt.profile}` : "",
		prompt.style ? `Style: ${prompt.style}` : "",
		prompt.accent ? `Accent: ${prompt.accent}` : "",
		prompt.pacing ? `Pacing: ${prompt.pacing}` : "",
	]
		.filter(Boolean)
		.join("\n");
}

async function testUnsavedPersona(text: string, draft: PersonaDraft): Promise<RpcResponse> {
	const params: Record<string, unknown> = { text };
	const instructions = personaTestInstructions(draft);
	if (instructions) params.instructions = instructions;
	if (draft.openaiVoice) params.voiceId = draft.openaiVoice;
	if (draft.openaiModel) params.model = draft.openaiModel;
	return (await sendRpc("tts.convert", params)) as RpcResponse;
}

async function personaTestResponse(
	isNew: boolean,
	editingId: string,
	text: string,
	draft: PersonaDraft,
	savePersona: () => Promise<boolean>,
): Promise<RpcResponse> {
	if (isNew) return testUnsavedPersona(text, draft);
	await savePersona();
	return (await testTtsWithPersona(text, editingId)) as RpcResponse;
}

function playPersonaTestResponse(response: RpcResponse): void {
	if (!response?.ok) return;
	const payload = response.payload as { audio?: string; mimeType?: string };
	if (!payload?.audio) return;
	const bytes = decodeBase64Safe(payload.audio);
	const blob = new Blob([bytes as BlobPart], { type: payload.mimeType || "audio/mpeg" });
	const url = URL.createObjectURL(blob);
	const audio = new Audio(url);
	audio.onended = () => URL.revokeObjectURL(url);
	audio.play().catch((error: Error) => console.error("[TTS]", error));
}

interface PersonaFieldProps {
	label: string;
	placeholder: string;
	value: string;
	onInput: (value: string) => void;
}

function PersonaField({ label, placeholder, value, onInput }: PersonaFieldProps): VNode {
	return (
		<label className="text-xs text-[var(--muted)] flex flex-col gap-1">
			{label}
			<input
				className="provider-key-input w-full"
				placeholder={placeholder}
				value={value}
				onInput={(event) => onInput(targetValue(event))}
			/>
		</label>
	);
}

function PersonaIdentityFields({
	isNew,
	draft,
	onChange,
}: {
	isNew: boolean;
	draft: PersonaDraft;
	onChange: PersonaDraftChange;
}): VNode {
	return (
		<>
			{isNew && (
				<PersonaField
					label="ID (lowercase, no spaces)"
					placeholder="alfred"
					value={draft.id}
					onInput={(value) => onChange("id", value)}
				/>
			)}
			<PersonaField
				label="Display Name"
				placeholder="Alfred the Butler"
				value={draft.label}
				onInput={(value) => onChange("label", value)}
			/>
			<PersonaField
				label="Description"
				placeholder="A wise British butler with dry wit"
				value={draft.description}
				onInput={(value) => onChange("description", value)}
			/>
		</>
	);
}

function PersonaPromptFields({ draft, onChange }: { draft: PersonaDraft; onChange: PersonaDraftChange }): VNode {
	return (
		<>
			<hr style={{ border: "none", borderTop: "1px solid var(--border)", margin: "4px 0" }} />
			<p className="text-xs text-[var(--muted)]" style={{ margin: 0 }}>
				Voice direction — controls tone on providers that support instructions (OpenAI gpt-4o-mini-tts).
			</p>
			<PersonaField
				label="Character Profile"
				placeholder="A wise British butler, dry wit, formal"
				value={draft.profile}
				onInput={(value) => onChange("profile", value)}
			/>
			<PersonaField
				label="Delivery Style"
				placeholder="Measured, deliberate, slightly amused"
				value={draft.style}
				onInput={(value) => onChange("style", value)}
			/>
			<PersonaField
				label="Accent"
				placeholder="Received Pronunciation"
				value={draft.accent}
				onInput={(value) => onChange("accent", value)}
			/>
			<PersonaField
				label="Pacing"
				placeholder="Unhurried, with dramatic pauses"
				value={draft.pacing}
				onInput={(value) => onChange("pacing", value)}
			/>
		</>
	);
}

function PersonaProviderBindings({ draft, onChange }: { draft: PersonaDraft; onChange: PersonaDraftChange }): VNode {
	return (
		<>
			<hr style={{ border: "none", borderTop: "1px solid var(--border)", margin: "4px 0" }} />
			<p className="text-xs text-[var(--muted)]" style={{ margin: 0 }}>
				Provider bindings — voice and model overrides per TTS provider.
			</p>
			<div
				className="flex flex-col gap-2 p-2 rounded border border-[var(--border)]"
				style={{ background: "var(--surface)" }}
			>
				<span className="text-xs font-medium text-[var(--text-strong)]">OpenAI TTS</span>
				<PersonaField
					label="Voice (alloy, echo, fable, onyx, nova, shimmer, coral, cedar, ...)"
					placeholder="alloy"
					value={draft.openaiVoice}
					onInput={(value) => onChange("openaiVoice", value)}
				/>
				<PersonaField
					label="Model"
					placeholder="gpt-4o-mini-tts"
					value={draft.openaiModel}
					onInput={(value) => onChange("openaiModel", value)}
				/>
			</div>
			<div
				className="flex flex-col gap-2 p-2 rounded border border-[var(--border)]"
				style={{ background: "var(--surface)" }}
			>
				<span className="text-xs font-medium text-[var(--text-strong)]">ElevenLabs</span>
				<PersonaField
					label="Voice ID"
					placeholder="21m00Tcm4TlvDq8ikWAM"
					value={draft.elevenVoice}
					onInput={(value) => onChange("elevenVoice", value)}
				/>
			</div>
		</>
	);
}

interface PersonaActionsProps {
	isNew: boolean;
	saving: boolean;
	testing: boolean;
	onTest: () => void;
	onClose: () => void;
	onSave: () => void;
}

function PersonaActions(props: PersonaActionsProps): VNode {
	return (
		<div className="flex gap-2 justify-end" style={{ marginTop: "8px" }}>
			<button
				type="button"
				className="provider-btn provider-btn-secondary"
				disabled={props.testing}
				onClick={props.onTest}
			>
				{props.testing ? "Testing..." : "Test Voice"}
			</button>
			<button type="button" className="provider-btn provider-btn-secondary" onClick={props.onClose}>
				Cancel
			</button>
			<button type="button" className="provider-btn" disabled={props.saving} onClick={props.onSave}>
				{props.saving ? "Saving..." : props.isNew ? "Create" : "Save"}
			</button>
		</div>
	);
}

export function PersonaEditModal({ editingId, existingPersona, onClose, onSaved }: PersonaEditModalProps): VNode {
	const isNew = editingId === "__new__";
	const [draft, setDraft] = useState<PersonaDraft>(() => initialPersonaDraft(existingPersona));
	const [saving, setSaving] = useState(false);
	const [testing, setTesting] = useState(false);
	const [error, setError] = useState<string | null>(null);

	function changeDraft(field: PersonaDraftField, value: string): void {
		setDraft((current) => ({ ...current, [field]: value }));
	}

	async function savePersona(): Promise<boolean> {
		setError(null);
		const validationError = personaValidationError(isNew, draft);
		if (validationError) {
			setError(validationError);
			return false;
		}
		setSaving(true);
		try {
			await persistPersona(isNew, editingId, draft);
			return true;
		} catch (caught) {
			setError(caught instanceof Error ? caught.message : String(caught));
			return false;
		} finally {
			setSaving(false);
		}
	}

	async function handleSave(): Promise<void> {
		if (await savePersona()) onSaved();
	}

	async function handleTest(): Promise<void> {
		setTesting(true);
		try {
			const identity = gon.get("identity") as { user_name?: string; name?: string } | undefined;
			const text = await fetchPhrase(
				"settings",
				identity?.user_name || "friend",
				draft.label || identity?.name || "Chelix",
			);
			playPersonaTestResponse(await personaTestResponse(isNew, editingId, text, draft, savePersona));
		} catch (_error) {
			// Voice testing is best-effort; save errors remain visible in the modal.
		} finally {
			setTesting(false);
		}
	}

	return (
		<Modal show onClose={onClose} title={isNew ? "New Voice Persona" : `Edit ${draft.label}`}>
			<div
				className="channel-form"
				style={{
					display: "flex",
					flexDirection: "column",
					gap: "12px",
					padding: "16px",
					maxHeight: "70vh",
					overflowY: "auto",
				}}
			>
				<PersonaIdentityFields isNew={isNew} draft={draft} onChange={changeDraft} />
				<PersonaPromptFields draft={draft} onChange={changeDraft} />
				<PersonaProviderBindings draft={draft} onChange={changeDraft} />
				{error && <div className="text-xs text-[var(--error)]">{error}</div>}
				<PersonaActions
					isNew={isNew}
					saving={saving}
					testing={testing}
					onTest={handleTest}
					onClose={onClose}
					onSave={handleSave}
				/>
			</div>
		</Modal>
	);
}

// ── LocalProviderInstructions (used only by AddVoiceProviderModal) ──

interface LocalProviderInstructionsProps {
	providerId: string;
	voxtralReqs: VoxtralRequirements | null;
}

const LOCAL_PROVIDER_TEMPLATE_IDS: Record<string, string> = {
	"whisper-cli": "voice-whisper-cli-instructions",
	"whisper-local": "voice-whisper-local-instructions",
	"sherpa-onnx": "voice-sherpa-onnx-instructions",
	piper: "voice-piper-instructions",
	coqui: "voice-coqui-instructions",
	"voxtral-local": "voice-voxtral-instructions",
};

function voxtralDetectedSummary(requirements: VoxtralRequirements): string {
	const python = requirements.python?.available ? `Python ${requirements.python.version}` : "no Python";
	const cuda = requirements.cuda?.available
		? `${requirements.cuda.gpu_name || "NVIDIA GPU"} (${Math.round((requirements.cuda.memory_mb || 0) / 1024)}GB)`
		: "no CUDA GPU";
	return `${requirements.os}/${requirements.arch}, ${python}, ${cuda}`;
}

function appendVoxtralReasons(element: HTMLElement, requirements: VoxtralRequirements): void {
	if (requirements.compatible || !requirements.reasons?.length) return;
	const list = element.querySelector<HTMLElement>("[data-voxtral-reasons]");
	if (!list) return;
	for (const reason of requirements.reasons) {
		const item = document.createElement("li");
		item.style.margin = "2px 0";
		item.textContent = reason;
		list.appendChild(item);
	}
}

function voxtralRequirementsElement(requirements: VoxtralRequirements | null): HTMLElement {
	if (!requirements) {
		const loading = document.createElement("div");
		loading.className = "text-xs text-[var(--muted)] mb-3";
		loading.textContent = "Checking system requirements\u2026";
		return loading;
	}
	const element = cloneHidden(
		requirements.compatible ? "voice-voxtral-requirements-ok" : "voice-voxtral-requirements-fail",
	);
	if (!element) return document.createElement("div");
	const detected = element.querySelector<HTMLElement>("[data-voxtral-detected]");
	if (detected) detected.textContent = voxtralDetectedSummary(requirements);
	appendVoxtralReasons(element, requirements);
	return element;
}

function localProviderInstructions(providerId: string, requirements: VoxtralRequirements | null): HTMLElement | null {
	const templateId = LOCAL_PROVIDER_TEMPLATE_IDS[providerId];
	if (!templateId) return null;
	const instructions = cloneHidden(templateId);
	if (!instructions || providerId !== "voxtral-local") return instructions;
	const requirementsContainer = instructions.querySelector<HTMLElement>("[data-voxtral-requirements]");
	if (requirementsContainer) requirementsContainer.appendChild(voxtralRequirementsElement(requirements));
	return instructions;
}

function LocalProviderInstructions({ providerId, voxtralReqs }: LocalProviderInstructionsProps): VNode {
	const ref = useRef<HTMLDivElement>(null);

	useEffect(() => {
		const container = ref.current;
		if (!container) return;
		const instructions = localProviderInstructions(providerId, voxtralReqs);
		container.replaceChildren(...(instructions ? [instructions] : []));
	}, [providerId, voxtralReqs]);

	return <div ref={ref} />;
}

// ── AddVoiceProviderModal ───────────────────────────────────

interface AddVoiceProviderModalProps {
	unconfiguredProviders: VoiceProviderData[];
	voxtralReqs: VoxtralRequirements | null;
	onSaved: () => void;
}

interface ElevenlabsCatalog {
	voices: { id: string; name: string }[];
	models: { id: string; name: string }[];
	warning: string | null;
}

interface VoiceProviderDraft {
	apiKey: string;
	baseUrl: string;
	voice: string;
	model: string;
	languageCode: string;
}

type VoiceProviderDraftField = keyof VoiceProviderDraft;
type VoiceProviderDraftChange = (field: VoiceProviderDraftField, value: string) => void;

interface VoiceProviderCapabilities {
	isElevenLabs: boolean;
	supportsTtsVoiceSettings: boolean;
	supportsBaseUrl: boolean;
	supportsModelSettings: boolean;
	modelChoices: string[];
	realtimeModelChoices: string[];
}

interface VoiceSaveOptions {
	baseUrl?: string;
	voice?: string;
	model?: string;
	languageCode?: string;
}

type VoiceSaveRequest =
	| { ok: true; providerId: string; apiKey: string | null; options: VoiceSaveOptions }
	| { ok: false; error: string };

const EMPTY_ELEVENLABS_CATALOG: ElevenlabsCatalog = { voices: [], models: [], warning: null };

function emptyVoiceProviderDraft(): VoiceProviderDraft {
	return { apiKey: "", baseUrl: "", voice: "", model: "", languageCode: "" };
}

function selectedVoiceProvider(
	selectedProvider: string | null,
	unconfiguredProviders: VoiceProviderData[],
	selectedData: VoiceProviderData | null,
): VoiceProviderData | null {
	if (!selectedProvider) return null;
	return unconfiguredProviders.find((provider) => provider.id === selectedProvider) || selectedData;
}

function voiceProviderCapabilities(
	providerId: string | null,
	meta: VoiceProviderData | null,
): VoiceProviderCapabilities {
	const supportsTtsVoiceSettings = meta?.type === "tts";
	return {
		isElevenLabs: providerId === "elevenlabs" || providerId === "elevenlabs-stt",
		supportsTtsVoiceSettings,
		supportsBaseUrl: meta?.capabilities?.baseUrl === true,
		supportsModelSettings: supportsTtsVoiceSettings || meta?.capabilities?.customModel === true,
		modelChoices: meta?.capabilities?.modelChoices || [],
		realtimeModelChoices: meta?.capabilities?.realtimeModelChoices || [],
	};
}

function configuredSetting(value: string | undefined): boolean {
	return typeof value === "string" && value.trim().length > 0;
}

interface VoiceSettingsPresence {
	baseUrl: boolean;
	model: boolean;
	any: boolean;
}

function voiceSettingsPresence(
	meta: VoiceProviderData | null,
	capabilities: VoiceProviderCapabilities,
	draft: VoiceProviderDraft,
): VoiceSettingsPresence {
	const baseUrl =
		capabilities.supportsBaseUrl && (draft.baseUrl.trim().length > 0 || configuredSetting(meta?.settings?.baseUrl));
	const model =
		capabilities.supportsModelSettings && (draft.model.trim().length > 0 || configuredSetting(meta?.settings?.model));
	const tts = capabilities.supportsTtsVoiceSettings && Boolean(draft.voice.trim() || draft.languageCode.trim());
	return { baseUrl, model, any: tts || model || baseUrl };
}

function voiceSaveRequest(
	providerId: string | null,
	meta: VoiceProviderData | null,
	capabilities: VoiceProviderCapabilities,
	draft: VoiceProviderDraft,
): VoiceSaveRequest {
	if (!providerId) return { ok: false, error: "Select a voice provider." };
	const apiKey = draft.apiKey.trim();
	const presence = voiceSettingsPresence(meta, capabilities, draft);
	if (!(apiKey || presence.any)) {
		return { ok: false, error: "Provide an API key, base URL, or at least one provider setting." };
	}
	return {
		ok: true,
		providerId,
		apiKey: apiKey || null,
		options: {
			baseUrl: presence.baseUrl ? draft.baseUrl.trim() : undefined,
			voice: capabilities.supportsTtsVoiceSettings ? draft.voice.trim() || undefined : undefined,
			model: presence.model ? draft.model.trim() : undefined,
			languageCode: capabilities.supportsTtsVoiceSettings ? draft.languageCode.trim() || undefined : undefined,
		},
	};
}

async function saveVoiceProvider(request: Extract<VoiceSaveRequest, { ok: true }>): Promise<RpcResponse> {
	const response = request.apiKey
		? await saveVoiceKey(request.providerId, request.apiKey, request.options)
		: await saveVoiceSettings(request.providerId, request.options);
	return response as RpcResponse;
}

function voiceSaveError(response: RpcResponse): string {
	return response.error?.message || "Failed to save key";
}

async function loadElevenlabsCatalog(): Promise<ElevenlabsCatalog | null> {
	const response = (await sendRpc("voice.elevenlabs.catalog", {})) as RpcResponse;
	if (!response?.ok) return null;
	const payload = response.payload as {
		voices?: { id: string; name: string }[];
		models?: { id: string; name: string }[];
		warning?: string;
	};
	return {
		voices: payload?.voices || [],
		models: payload?.models || [],
		warning: payload?.warning || null,
	};
}

interface CloudProviderModalProps {
	providerId: string;
	meta: VoiceProviderData;
	draft: VoiceProviderDraft;
	capabilities: VoiceProviderCapabilities;
	catalog: ElevenlabsCatalog;
	catalogLoading: boolean;
	saving: boolean;
	error: string;
	onChange: VoiceProviderDraftChange;
	onBack: () => void;
	onClose: () => void;
	onSave: () => void;
}

function ProviderApiKeyField({
	meta,
	value,
	onInput,
}: {
	meta: VoiceProviderData;
	value: string;
	onInput: (value: string) => void;
}): VNode {
	return (
		<>
			<label>
				<span className="text-xs text-[var(--muted)]">API Key</span>
				<input
					type="password"
					className="provider-key-input w-full"
					value={value}
					onInput={(event) => onInput(targetValue(event))}
					placeholder={meta.keyPlaceholder || "Leave blank to keep existing key"}
				/>
			</label>
			{meta.keyUrl && (
				<div className="text-xs text-[var(--muted)]">
					Get your API key at{` `}
					<a href={meta.keyUrl} target="_blank" rel="noopener" className="hover:underline text-[var(--accent)]">
						{meta.keyUrlLabel}
					</a>
				</div>
			)}
		</>
	);
}

function ProviderBaseUrlField({
	visible,
	value,
	onInput,
}: {
	visible: boolean;
	value: string;
	onInput: (value: string) => void;
}): VNode | null {
	if (!visible) return null;
	return (
		<div className="mt-2 flex flex-col gap-2">
			<label>
				<span className="text-xs text-[var(--muted)]">Base URL</span>
				<input
					type="text"
					className="provider-key-input w-full"
					data-field="baseUrl"
					value={value}
					onInput={(event) => onInput(targetValue(event))}
					placeholder="http://localhost:8000/v1"
				/>
			</label>
			<div className="text-xs text-[var(--muted)]">
				Use this for a local or OpenAI-compatible server. Leave the API key blank if your endpoint does not require one.
			</div>
		</div>
	);
}

interface ProviderVoiceFieldProps {
	visible: boolean;
	isElevenLabs: boolean;
	catalog: ElevenlabsCatalog;
	catalogLoading: boolean;
	value: string;
	onInput: (value: string) => void;
}

function ProviderVoiceField(props: ProviderVoiceFieldProps): VNode | null {
	if (!props.visible) return null;
	return (
		<div className="flex flex-col gap-2">
			<span className="text-xs text-[var(--muted)]">Voice</span>
			{props.isElevenLabs && props.catalogLoading && (
				<div className="text-xs text-[var(--muted)]">Loading ElevenLabs voices...</div>
			)}
			{props.isElevenLabs && props.catalog.warning && (
				<div className="text-xs text-[var(--muted)]">{props.catalog.warning}</div>
			)}
			{props.isElevenLabs && props.catalog.voices.length > 0 && (
				<select
					className="provider-key-input w-full"
					aria-label="Voice catalog"
					onChange={(event) => props.onInput(targetValue(event))}
				>
					<option value="">Pick a voice from your account...</option>
					{props.catalog.voices.map((voice) => (
						<option key={voice.id} value={voice.id}>
							{voice.name} ({voice.id})
						</option>
					))}
				</select>
			)}
			<input
				type="text"
				aria-label="Voice"
				className="provider-key-input w-full"
				value={props.value}
				onInput={(event) => props.onInput(targetValue(event))}
				list={props.isElevenLabs ? "elevenlabs-voice-options" : undefined}
				placeholder="voice id / name (optional)"
			/>
			{props.isElevenLabs && (
				<datalist id="elevenlabs-voice-options">
					{props.catalog.voices.map((voice) => (
						<option key={voice.id} value={voice.id}>
							{voice.name}
						</option>
					))}
				</datalist>
			)}
		</div>
	);
}

interface ProviderModelFieldProps {
	visible: boolean;
	isElevenLabs: boolean;
	catalog: ElevenlabsCatalog;
	modelChoices: string[];
	realtimeModelChoices: string[];
	value: string;
	onInput: (value: string) => void;
}

function ProviderModelField(props: ProviderModelFieldProps): VNode | null {
	if (!props.visible) return null;
	const listId = props.isElevenLabs
		? "elevenlabs-model-options"
		: props.modelChoices.length > 0
			? "voice-model-options"
			: undefined;
	return (
		<div className="flex flex-col gap-2">
			<span className="text-xs text-[var(--muted)]">Model</span>
			{props.isElevenLabs && props.catalog.models.length > 0 && (
				<select
					className="provider-key-input w-full"
					aria-label="Model catalog"
					onChange={(event) => props.onInput(targetValue(event))}
				>
					<option value="">Pick a model...</option>
					{props.catalog.models.map((model) => (
						<option key={model.id} value={model.id}>
							{model.name} ({model.id})
						</option>
					))}
				</select>
			)}
			<input
				type="text"
				aria-label="Model"
				className="provider-key-input w-full"
				value={props.value}
				onInput={(event) => props.onInput(targetValue(event))}
				list={listId}
				placeholder="model (optional)"
			/>
			{props.isElevenLabs && (
				<datalist id="elevenlabs-model-options">
					{props.catalog.models.map((model) => (
						<option key={model.id} value={model.id}>
							{model.name}
						</option>
					))}
				</datalist>
			)}
			{!props.isElevenLabs && props.modelChoices.length > 0 && (
				<datalist id="voice-model-options">
					{props.modelChoices.map((model) => (
						<option key={model} value={model} />
					))}
				</datalist>
			)}
			{props.realtimeModelChoices.length > 0 && (
				<div className="rounded border border-[var(--border)] bg-[var(--surface2)] px-3 py-2 text-xs text-[var(--muted)]">
					OpenAI Realtime models: {props.realtimeModelChoices.join(", ")}. These use the Realtime API, not this
					record-and-transcribe provider.
				</div>
			)}
		</div>
	);
}

function ProviderLanguageField({
	visible,
	value,
	onInput,
}: {
	visible: boolean;
	value: string;
	onInput: (value: string) => void;
}): VNode | null {
	if (!visible) return null;
	return (
		<div className="flex flex-col gap-2">
			<label>
				<span className="text-xs text-[var(--muted)]">Language Code</span>
				<input
					type="text"
					className="provider-key-input w-full"
					value={value}
					onInput={(event) => onInput(targetValue(event))}
					placeholder="en-US (optional)"
				/>
			</label>
		</div>
	);
}

function ProviderHint({ hint }: { hint?: string }): VNode | null {
	if (!hint) return null;
	return (
		<div
			className="text-xs text-[var(--muted)]"
			style={{
				marginTop: "8px",
				padding: "8px",
				background: "var(--surface-alt)",
				borderRadius: "4px",
				fontStyle: "italic",
			}}
		>
			{hint}
		</div>
	);
}

function CloudProviderModal(props: CloudProviderModalProps): VNode {
	const isGoogle = props.providerId === "google" || props.providerId === "google-tts";
	return (
		<Modal show={voiceShowAddModal.value} onClose={props.onClose} title={`Add ${props.meta.name}`}>
			<div className="channel-form">
				<div className="text-sm text-[var(--text-strong)]">{props.meta.name}</div>
				<div className="mb-3 text-xs text-[var(--muted)]">{props.meta.description}</div>
				<ProviderApiKeyField
					meta={props.meta}
					value={props.draft.apiKey}
					onInput={(value) => props.onChange("apiKey", value)}
				/>
				<ProviderBaseUrlField
					visible={props.capabilities.supportsBaseUrl}
					value={props.draft.baseUrl}
					onInput={(value) => props.onChange("baseUrl", value)}
				/>
				<ProviderVoiceField
					visible={props.capabilities.supportsTtsVoiceSettings}
					isElevenLabs={props.capabilities.isElevenLabs}
					catalog={props.catalog}
					catalogLoading={props.catalogLoading}
					value={props.draft.voice}
					onInput={(value) => props.onChange("voice", value)}
				/>
				<ProviderModelField
					visible={props.capabilities.supportsModelSettings}
					isElevenLabs={props.capabilities.isElevenLabs}
					catalog={props.catalog}
					modelChoices={props.capabilities.modelChoices}
					realtimeModelChoices={props.capabilities.realtimeModelChoices}
					value={props.draft.model}
					onInput={(value) => props.onChange("model", value)}
				/>
				<ProviderLanguageField
					visible={props.capabilities.supportsTtsVoiceSettings && isGoogle}
					value={props.draft.languageCode}
					onInput={(value) => props.onChange("languageCode", value)}
				/>
				<ProviderHint hint={props.meta.hint} />
				{props.error && <div className="text-xs text-[var(--error)]">{props.error}</div>}
				<div className="flex gap-2 mt-2">
					<button type="button" className="provider-btn provider-btn-secondary" onClick={props.onBack}>
						Back
					</button>
					<button type="button" className="provider-btn" disabled={props.saving} onClick={props.onSave}>
						{props.saving ? "Saving\u2026" : "Save"}
					</button>
				</div>
			</div>
		</Modal>
	);
}

function LocalProviderModal({
	providerId,
	meta,
	voxtralReqs,
	onBack,
	onClose,
}: {
	providerId: string;
	meta: VoiceProviderData;
	voxtralReqs: VoxtralRequirements | null;
	onBack: () => void;
	onClose: () => void;
}): VNode {
	return (
		<Modal show={voiceShowAddModal.value} onClose={onClose} title={`Add ${meta.name}`}>
			<div className="channel-form">
				<div className="text-sm text-[var(--text-strong)]">{meta.name}</div>
				<div className="text-xs text-[var(--muted)] mb-3">{meta.description}</div>
				<LocalProviderInstructions providerId={providerId} voxtralReqs={voxtralReqs} />
				<div className="flex gap-2 mt-3">
					<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
						Back
					</button>
				</div>
			</div>
		</Modal>
	);
}

function ProviderSelectionButton({
	provider,
	onSelect,
}: {
	provider: VoiceProviderData;
	onSelect: (id: string) => void;
}): VNode {
	return (
		<button
			type="button"
			className="provider-card"
			style={{
				padding: "10px 12px",
				borderRadius: "6px",
				cursor: "pointer",
				textAlign: "left",
				border: "1px solid var(--border)",
				background: "var(--surface)",
			}}
			onClick={() => onSelect(provider.id)}
		>
			<div className="flex items-center gap-2">
				<div className="flex-1">
					<div className="text-sm text-[var(--text-strong)]">{provider.name}</div>
					<div className="text-xs text-[var(--muted)]">{provider.description}</div>
				</div>
				<span className="icon icon-chevron-right text-[var(--muted)]" />
			</div>
		</button>
	);
}

function ProviderSelectionGroup({
	title,
	providers,
	onSelect,
}: {
	title: string;
	providers: VoiceProviderData[];
	onSelect: (id: string) => void;
}): VNode | null {
	if (providers.length === 0) return null;
	return (
		<div>
			<h4 className="text-xs font-medium text-[var(--muted)] m-0 mb-2 uppercase tracking-[0.5px]">{title}</h4>
			<div className="flex flex-col gap-1.5">
				{providers.map((provider) => (
					<ProviderSelectionButton key={provider.id} provider={provider} onSelect={onSelect} />
				))}
			</div>
		</div>
	);
}

function ProviderSelectionModal({
	providers,
	onSelect,
	onClose,
}: {
	providers: VoiceProviderData[];
	onSelect: (id: string) => void;
	onClose: () => void;
}): VNode {
	const groups = [
		{
			title: "Speech-to-Text (Cloud)",
			providers: providers.filter((provider) => provider.type === "stt" && provider.category === "cloud"),
		},
		{
			title: "Speech-to-Text (Local)",
			providers: providers.filter((provider) => provider.type === "stt" && provider.category === "local"),
		},
		{ title: "Text-to-Speech", providers: providers.filter((provider) => provider.type === "tts") },
	];
	return (
		<Modal show={voiceShowAddModal.value} onClose={onClose} title="Add Voice Provider">
			<div className="channel-form gap-4">
				{groups.map((group) => (
					<ProviderSelectionGroup key={group.title} {...group} onSelect={onSelect} />
				))}
				{providers.length === 0 && (
					<div className="text-sm text-[var(--muted)] text-center py-5">
						All available providers are already configured.
					</div>
				)}
			</div>
		</Modal>
	);
}

interface AddVoiceProviderViewProps {
	selectedProvider: string | null;
	providerMeta: VoiceProviderData | null;
	unconfiguredProviders: VoiceProviderData[];
	voxtralReqs: VoxtralRequirements | null;
	draft: VoiceProviderDraft;
	capabilities: VoiceProviderCapabilities;
	catalog: ElevenlabsCatalog;
	catalogLoading: boolean;
	saving: boolean;
	error: string;
	onChange: VoiceProviderDraftChange;
	onSelect: (providerId: string) => void;
	onCloudBack: () => void;
	onLocalBack: () => void;
	onClose: () => void;
	onSave: () => void;
}

function AddVoiceProviderView(props: AddVoiceProviderViewProps): VNode {
	if (props.selectedProvider && props.providerMeta?.category === "cloud") {
		return (
			<CloudProviderModal
				providerId={props.selectedProvider}
				meta={props.providerMeta}
				draft={props.draft}
				capabilities={props.capabilities}
				catalog={props.catalog}
				catalogLoading={props.catalogLoading}
				saving={props.saving}
				error={props.error}
				onChange={props.onChange}
				onBack={props.onCloudBack}
				onClose={props.onClose}
				onSave={props.onSave}
			/>
		);
	}
	if (props.selectedProvider && props.providerMeta?.category === "local") {
		return (
			<LocalProviderModal
				providerId={props.selectedProvider}
				meta={props.providerMeta}
				voxtralReqs={props.voxtralReqs}
				onBack={props.onLocalBack}
				onClose={props.onClose}
			/>
		);
	}
	return (
		<ProviderSelectionModal providers={props.unconfiguredProviders} onSelect={props.onSelect} onClose={props.onClose} />
	);
}

export function AddVoiceProviderModal({
	unconfiguredProviders,
	voxtralReqs,
	onSaved,
}: AddVoiceProviderModalProps): VNode {
	const [draft, setDraft] = useState<VoiceProviderDraft>(emptyVoiceProviderDraft);
	const [elevenlabsCatalog, setElevenlabsCatalog] = useState<ElevenlabsCatalog>(EMPTY_ELEVENLABS_CATALOG);
	const [elevenlabsCatalogLoading, setElevenlabsCatalogLoading] = useState(false);
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState("");
	const selectedProvider = voiceSelectedProvider.value;
	const selectedData = voiceSelectedProviderData.value;
	const providerMeta = selectedVoiceProvider(selectedProvider, unconfiguredProviders, selectedData);
	const capabilities = voiceProviderCapabilities(selectedProvider, providerMeta);

	function changeDraft(field: VoiceProviderDraftField, value: string): void {
		setDraft((current) => ({ ...current, [field]: value }));
	}

	function onClose(): void {
		voiceShowAddModal.value = false;
		voiceSelectedProvider.value = null;
		voiceSelectedProviderData.value = null;
		setDraft(emptyVoiceProviderDraft());
		setError("");
	}

	function onSelectProvider(providerId: string): void {
		voiceSelectedProvider.value = providerId;
		voiceSelectedProviderData.value = null;
		setDraft(emptyVoiceProviderDraft());
		setError("");
	}

	function onCloudBack(): void {
		voiceSelectedProvider.value = null;
		setDraft((current) => ({ ...current, apiKey: "" }));
		setError("");
	}

	function onLocalBack(): void {
		voiceSelectedProvider.value = null;
	}

	async function onSave(): Promise<void> {
		const request = voiceSaveRequest(selectedProvider, providerMeta, capabilities, draft);
		if (!request.ok) {
			setError(request.error);
			return;
		}
		setError("");
		setSaving(true);
		try {
			const response = await saveVoiceProvider(request);
			if (!response.ok) {
				setError(voiceSaveError(response));
				return;
			}
			setDraft((current) => ({ ...current, apiKey: "" }));
			onSaved();
		} catch (caught) {
			setError(caught instanceof Error ? caught.message : String(caught));
		} finally {
			setSaving(false);
		}
	}

	useEffect(() => {
		const settings = selectedData?.settings;
		if (!settings) return;
		setDraft((current) => ({
			...current,
			baseUrl: settings.baseUrl || "",
			voice: settings.voiceId || settings.voice || "",
			model: settings.model || "",
			languageCode: settings.languageCode || "",
		}));
	}, [selectedProvider, selectedData]);

	useEffect(() => {
		if (!capabilities.isElevenLabs) {
			setElevenlabsCatalog(EMPTY_ELEVENLABS_CATALOG);
			return;
		}
		setElevenlabsCatalogLoading(true);
		loadElevenlabsCatalog()
			.then((catalog) => {
				if (catalog) setElevenlabsCatalog(catalog);
			})
			.catch(() => {
				setElevenlabsCatalog({ voices: [], models: [], warning: "Failed to fetch ElevenLabs voice catalog." });
			})
			.finally(() => {
				setElevenlabsCatalogLoading(false);
				rerender();
			});
	}, [selectedProvider, capabilities.isElevenLabs]);

	return (
		<AddVoiceProviderView
			selectedProvider={selectedProvider}
			providerMeta={providerMeta}
			unconfiguredProviders={unconfiguredProviders}
			voxtralReqs={voxtralReqs}
			draft={draft}
			capabilities={capabilities}
			catalog={elevenlabsCatalog}
			catalogLoading={elevenlabsCatalogLoading}
			saving={saving}
			error={error}
			onChange={changeDraft}
			onSelect={onSelectProvider}
			onCloudBack={onCloudBack}
			onLocalBack={onLocalBack}
			onClose={onClose}
			onSave={onSave}
		/>
	);
}
