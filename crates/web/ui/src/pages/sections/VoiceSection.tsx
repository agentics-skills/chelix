// ── Voice section ────────────────────────────────────────────

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { TabBar } from "../../components/forms/Tabs";
import * as gon from "../../gon";
import { sendRpc } from "../../helpers";
import { connected } from "../../signals";
import * as S from "../../state";
import { fetchPhrase } from "../../tts-phrases";
import { targetChecked, targetValue } from "../../typed-events";
import { showToast } from "../../ui";
import { getPttKey, getVadSensitivity, setPttKey, setVadSensitivity } from "../../voice-input";
import {
	decodeBase64Safe,
	deleteVoicePersona,
	fetchVoiceProviders,
	listVoicePersonas,
	setActiveVoicePersona,
	testTts,
	testTtsWithPersona,
	toggleVoiceProvider,
	transcribeAudio,
	type VoicePersonaResponse,
} from "../../voice-utils";
import type { RpcResponse } from "./_shared";
import { rerender } from "./_shared";
import {
	AddVoiceProviderModal,
	PersonaEditModal,
	type VoiceProviderData,
	type VoiceTesting,
	type VoiceTestResult,
	type VoxtralRequirements,
	voiceSelectedProvider,
	voiceSelectedProviderData,
	voiceShowAddModal,
} from "./VoiceModals";

interface VoiceProviders {
	tts: VoiceProviderData[];
	stt: VoiceProviderData[];
}

interface PttKeyPickerProps {
	pttListening: boolean;
	setPttListening: (v: boolean) => void;
	pttKeyValue: string;
	setPttKeyValue: (v: string) => void;
}

function PttKeyPicker({ pttListening, setPttListening, pttKeyValue, setPttKeyValue }: PttKeyPickerProps): VNode {
	const handlerRef = useRef<((ev: KeyboardEvent) => void) | null>(null);

	useEffect(() => {
		return () => {
			if (handlerRef.current) {
				document.removeEventListener("keydown", handlerRef.current, true);
				handlerRef.current = null;
			}
		};
	}, []);

	return (
		<button
			type="button"
			className="provider-key-input"
			style={{ minWidth: "120px", textAlign: "center", cursor: "pointer" }}
			onClick={() => {
				if (pttListening) return;
				setPttListening(true);
				const handler = (ev: KeyboardEvent): void => {
					ev.preventDefault();
					ev.stopPropagation();
					setPttKeyValue(ev.key);
					setPttKey(ev.key);
					setPttListening(false);
					document.removeEventListener("keydown", handler, true);
					handlerRef.current = null;
					rerender();
				};
				handlerRef.current = handler;
				document.addEventListener("keydown", handler, true);
				rerender();
			}}
		>
			{pttListening ? "Press any key..." : pttKeyValue}
		</button>
	);
}

interface VoiceAudioPayload {
	audio: string;
	mimeType?: string;
	content_type?: string;
}

interface SttUploadPayload {
	ok?: boolean;
	transcription?: { text?: string };
	transcriptionError?: string;
	error?: string;
}

interface SttRecordingCallbacks {
	onTranscribing: () => void;
	onResult: (result: VoiceTestResult) => void;
}

function settingsVoiceIdentity(): { user: string; bot: string } {
	const identity = gon.get("identity") as { user_name?: string; name?: string } | undefined;
	return { user: identity?.user_name || "friend", bot: identity?.name || "Chelix" };
}

function playProviderTestAudio(payload: VoiceAudioPayload): void {
	const bytes = decodeBase64Safe(payload.audio);
	const blob = new Blob([bytes as BlobPart], { type: payload.mimeType || payload.content_type || "audio/mpeg" });
	const url = URL.createObjectURL(blob);
	const audio = new Audio(url);
	audio.onerror = (event) => {
		console.error("[TTS] audio element error:", audio.error?.message || event);
		URL.revokeObjectURL(url);
	};
	audio.onended = () => URL.revokeObjectURL(url);
	audio.play().catch((error: Error) => console.error("[TTS] play() failed:", error));
}

async function runTtsProviderTest(providerId: string): Promise<VoiceTestResult> {
	try {
		const identity = settingsVoiceIdentity();
		const text = await fetchPhrase("settings", identity.user, identity.bot);
		const response = (await testTts(text, providerId)) as RpcResponse;
		const payload = response.payload as VoiceAudioPayload | undefined;
		if (!(response.ok && payload?.audio)) {
			return { success: false, error: response.error?.message || "TTS test failed" };
		}
		playProviderTestAudio(payload);
		return { success: true, error: null };
	} catch (caught) {
		return { success: false, error: caught instanceof Error ? caught.message : "TTS test failed" };
	}
}

function sttUploadResult(payload: SttUploadPayload): VoiceTestResult {
	if (payload.ok && typeof payload.transcription?.text === "string") {
		const text = payload.transcription.text.trim();
		return { text: text || null, error: text ? null : "No speech detected" };
	}
	return { text: null, error: payload.transcriptionError || payload.error || "STT test failed" };
}

function sttHttpError(body: string): string {
	try {
		return (JSON.parse(body) as { error?: string }).error || "STT test failed";
	} catch (_error) {
		return "STT test failed";
	}
}

async function transcribeProviderAudio(providerId: string, audio: Blob): Promise<VoiceTestResult> {
	try {
		const response = await transcribeAudio(S.activeSessionKey, providerId, audio);
		if (response.ok) return sttUploadResult((await response.json()) as SttUploadPayload);
		const body = await response.text();
		console.error("[STT] upload failed: status=%d body=%s", response.status, body);
		return { text: null, error: `${sttHttpError(body)} (HTTP ${response.status})` };
	} catch (caught) {
		return { text: null, error: caught instanceof Error ? caught.message : "STT test failed" };
	}
}

function recordingMimeType(): string {
	return MediaRecorder.isTypeSupported("audio/webm;codecs=opus") ? "audio/webm;codecs=opus" : "audio/webm";
}

async function finishSttProviderRecording(
	providerId: string,
	recorder: MediaRecorder,
	stream: MediaStream,
	chunks: Blob[],
	fallbackMimeType: string,
	callbacks: SttRecordingCallbacks,
): Promise<void> {
	callbacks.onTranscribing();
	for (const track of stream.getTracks()) track.stop();
	const audio = new Blob(chunks, { type: recorder.mimeType || fallbackMimeType });
	callbacks.onResult(await transcribeProviderAudio(providerId, audio));
}

async function startSttProviderRecording(providerId: string, callbacks: SttRecordingCallbacks): Promise<MediaRecorder> {
	const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
	const mimeType = recordingMimeType();
	const recorder = new MediaRecorder(stream, { mimeType });
	const chunks: Blob[] = [];
	recorder.ondataavailable = (event: BlobEvent) => {
		if (event.data.size > 0) chunks.push(event.data);
	};
	recorder.onstop = () => {
		void finishSttProviderRecording(providerId, recorder, stream, chunks, mimeType, callbacks);
	};
	recorder.start();
	return recorder;
}

function humanizeMicError(error: { name?: string; message?: string }): string {
	if (error.name === "OverconstrainedError" || (error.message && /constraint/i.test(error.message))) {
		return "No compatible microphone found. Check your audio input device.";
	}
	if (error.name === "NotFoundError" || error.name === "NotAllowedError") {
		return "Microphone access denied or no microphone found. Check browser permissions.";
	}
	if (error.name === "NotReadableError") return "Microphone is in use by another application.";
	return error.message || "STT test failed";
}

type PersonaTestPhase = "testing" | "playing" | "";

interface PersonaTestCallbacks {
	onPlaying: () => void;
	onEnded: () => void;
}

function playPersonaAudio(payload: { audio: string; mimeType?: string }, onEnded: () => void): void {
	const bytes = decodeBase64Safe(payload.audio);
	const blob = new Blob([bytes as BlobPart], { type: payload.mimeType || "audio/mpeg" });
	const url = URL.createObjectURL(blob);
	const audio = new Audio(url);
	audio.onended = () => {
		URL.revokeObjectURL(url);
		onEnded();
	};
	audio.play().catch((error: Error) => console.error("[TTS]", error));
}

async function startPersonaTest(personaId: string, callbacks: PersonaTestCallbacks): Promise<boolean> {
	try {
		const identity = settingsVoiceIdentity();
		const text = await fetchPhrase("settings", identity.user, identity.bot);
		const response = (await testTtsWithPersona(text, personaId)) as RpcResponse;
		const payload = response.payload as { audio?: string; mimeType?: string } | undefined;
		if (!(response.ok && payload?.audio)) return false;
		callbacks.onPlaying();
		playPersonaAudio({ ...payload, audio: payload.audio }, callbacks.onEnded);
		return true;
	} catch (_error) {
		return false;
	}
}

function personaTestLabel(phase: PersonaTestPhase): string {
	if (phase === "testing") return "Testing\u2026";
	if (phase === "playing") return "Playing\u2026";
	return "Test";
}

function PersonaBindingBadges({ persona }: { persona: VoicePersonaResponse }): VNode {
	return (
		<>
			{persona.persona.provider_bindings.map((binding) => (
				<span
					key={binding.provider}
					className="text-[10px] px-1.5 py-0.5 rounded bg-[var(--surface-alt)] text-[var(--muted)]"
				>
					{binding.provider}
					{binding.voice_id ? `: ${binding.voice_id}` : ""}
				</span>
			))}
		</>
	);
}

function PersonaDetails({ persona }: { persona: VoicePersonaResponse }): VNode {
	return (
		<div className="flex-1 min-w-0">
			<div className="flex items-center gap-2 flex-wrap">
				<span className="text-sm font-medium text-[var(--text-strong)]">{persona.persona.label}</span>
				{persona.isActive && (
					<span className="text-[10px] px-1.5 py-0.5 rounded bg-[var(--accent)] text-white">active</span>
				)}
				<PersonaBindingBadges persona={persona} />
			</div>
			{persona.persona.description && (
				<p className="text-xs text-[var(--muted)] truncate mt-0.5 mb-0">{persona.persona.description}</p>
			)}
			{persona.persona.prompt.profile && (
				<p className="text-[10px] text-[var(--muted)] truncate italic mt-0.5 mb-0">{persona.persona.prompt.profile}</p>
			)}
		</div>
	);
}

interface PersonaActionsProps {
	persona: VoicePersonaResponse;
	testPhase: PersonaTestPhase;
	onTest: () => void;
	onEdit: () => void;
	onSetActive: (personaId: string | null) => void;
	onRemove: () => void;
}

function PersonaActions(props: PersonaActionsProps): VNode {
	const personaId = props.persona.persona.id;
	return (
		<div className="flex items-center gap-1.5">
			<button
				type="button"
				className="provider-btn provider-btn-secondary text-xs !py-1 !px-2.5"
				disabled={Boolean(props.testPhase)}
				onClick={props.onTest}
			>
				{personaTestLabel(props.testPhase)}
			</button>
			<button
				type="button"
				className="provider-btn provider-btn-secondary text-xs !py-1 !px-2.5"
				onClick={props.onEdit}
			>
				Edit
			</button>
			<button
				type="button"
				className="provider-btn provider-btn-secondary text-xs !py-1 !px-2.5"
				onClick={() => props.onSetActive(props.persona.isActive ? null : personaId)}
			>
				{props.persona.isActive ? "Deactivate" : "Activate"}
			</button>
			<button
				type="button"
				className="provider-btn text-xs !py-1 !px-2.5 !bg-[var(--error)] hover:!bg-red-700"
				onClick={props.onRemove}
			>
				Remove
			</button>
		</div>
	);
}

interface PersonaRowProps extends PersonaActionsProps {}

function PersonaRow(props: PersonaRowProps): VNode {
	return (
		<div
			className={`flex items-center gap-3 p-3 rounded border ${props.persona.isActive ? "border-[var(--accent)]" : "border-[var(--border)]"}`}
			style={{ background: "var(--surface)" }}
		>
			<PersonaDetails persona={props.persona} />
			<PersonaActions {...props} />
		</div>
	);
}

interface VoicePersonasPanelProps {
	personas: VoicePersonaResponse[];
	testing: Record<string, PersonaTestPhase>;
	editingId: string | null;
	onTest: (personaId: string) => void;
	onEdit: (personaId: string) => void;
	onSetActive: (personaId: string | null) => void;
	onRemove: (personaId: string) => void;
	onCloseEditor: () => void;
	onSaved: () => void;
}

function editedPersona(personas: VoicePersonaResponse[], editingId: string): VoicePersonaResponse | null {
	return editingId === "__new__" ? null : personas.find((persona) => persona.persona.id === editingId) || null;
}

function VoicePersonasPanel(props: VoicePersonasPanelProps): VNode {
	return (
		<div className="flex flex-col gap-3">
			<p className="text-xs text-[var(--muted)] leading-relaxed m-0">
				Named voice identities injected into every TTS call. Instead of improvising tone per-message, a persona defines
				a stable spoken character.
			</p>
			{props.personas.length === 0 ? (
				<p className="text-xs text-[var(--muted)] italic">No personas configured yet.</p>
			) : (
				<div className="flex flex-col gap-2">
					{props.personas.map((persona) => (
						<PersonaRow
							key={persona.persona.id}
							persona={persona}
							testPhase={props.testing[persona.persona.id] || ""}
							onTest={() => props.onTest(persona.persona.id)}
							onEdit={() => props.onEdit(persona.persona.id)}
							onSetActive={props.onSetActive}
							onRemove={() => props.onRemove(persona.persona.id)}
						/>
					))}
				</div>
			)}
			<button type="button" className="provider-btn" onClick={() => props.onEdit("__new__")}>
				+ Add Persona
			</button>
			{props.editingId && (
				<PersonaEditModal
					editingId={props.editingId}
					existingPersona={editedPersona(props.personas, props.editingId)}
					onClose={props.onCloseEditor}
					onSaved={props.onSaved}
				/>
			)}
		</div>
	);
}

export function VoiceSection(): VNode {
	const [allProviders, setAllProviders] = useState<VoiceProviders>({ tts: [], stt: [] });
	const [voiceLoading, setVoiceLoading] = useState(true);
	const [voxtralReqs, setVoxtralReqs] = useState<VoxtralRequirements | null>(null);
	const [savingProvider, setSavingProvider] = useState<string | null>(null);
	const [voiceTesting, setVoiceTesting] = useState<VoiceTesting | null>(null);
	const [activeRecorder, setActiveRecorder] = useState<MediaRecorder | null>(null);
	const [voiceTestResults, setVoiceTestResults] = useState<Record<string, VoiceTestResult>>({});

	// Tab state
	const [activeTab, setActiveTab] = useState("stt");

	// Per-persona test state: persona id → testing phase
	const [personaTesting, setPersonaTesting] = useState<Record<string, PersonaTestPhase>>({});

	// Voice personas
	const [personas, setPersonas] = useState<VoicePersonaResponse[]>([]);
	const [personaEditing, setPersonaEditing] = useState<string | null>(null);

	// PTT key configuration
	const [pttKeyValue, setPttKeyValue] = useState(getPttKey());
	const [pttListening, setPttListening] = useState(false);

	// VAD sensitivity
	const [vadSens, setVadSens] = useState(getVadSensitivity());

	function fetchVoiceStatus(options?: { silent?: boolean }): void {
		if (!options?.silent) {
			setVoiceLoading(true);
			rerender();
		}
		Promise.all([fetchVoiceProviders(), sendRpc("voice.config.voxtral_requirements", {})])
			.then(([providers, voxtral]) => {
				const provRes = providers as RpcResponse;
				const voxtralRes = voxtral as RpcResponse;
				if (provRes?.ok) setAllProviders((provRes.payload as VoiceProviders) || { tts: [], stt: [] });
				if (voxtralRes?.ok) setVoxtralReqs(voxtralRes.payload as VoxtralRequirements);
				if (!options?.silent) setVoiceLoading(false);
				rerender();
			})
			.catch(() => {
				if (!options?.silent) setVoiceLoading(false);
				rerender();
			});
	}

	async function fetchPersonas(): Promise<void> {
		try {
			const result = await listVoicePersonas();
			setPersonas(result.personas || []);
		} catch (_err) {
			/* ignore */
		}
	}

	useEffect(() => {
		if (connected.value) {
			fetchVoiceStatus();
			fetchPersonas();
		}
	}, [connected.value]);

	function onToggleProvider(provider: VoiceProviderData, enabled: boolean, providerType: string): void {
		setSavingProvider(provider.id);
		rerender();

		toggleVoiceProvider(provider.id, enabled, providerType)
			.then((r: unknown) => {
				const res = r as RpcResponse;
				setSavingProvider(null);
				if (res?.ok) {
					showToast(`${provider.name} ${enabled ? "enabled" : "disabled"}.`, "success");
					fetchVoiceStatus({ silent: true });
				} else {
					showToast((res?.error as { message?: string })?.message || "Failed to toggle provider", "error");
				}
				rerender();
			})
			.catch((err: Error) => {
				setSavingProvider(null);
				showToast(err.message, "error");
				rerender();
			});
	}

	function onConfigureProvider(providerId: string, providerData: VoiceProviderData): void {
		voiceSelectedProvider.value = providerId;
		voiceSelectedProviderData.value = providerData || null;
		voiceShowAddModal.value = true;
	}

	function getUnconfiguredProviders(): VoiceProviderData[] {
		return [...allProviders.stt, ...allProviders.tts].filter((p) => !p.available);
	}

	async function testVoiceProvider(providerId: string, type: string): Promise<void> {
		if (voiceTesting?.id === providerId && voiceTesting.type === "stt" && voiceTesting.phase === "recording") {
			activeRecorder?.stop();
			return;
		}

		setVoiceTesting({ id: providerId, type, phase: "testing" });
		rerender();
		if (type === "tts") {
			const result = await runTtsProviderTest(providerId);
			setVoiceTestResults((current) => ({ ...current, [providerId]: result }));
			setVoiceTesting(null);
			rerender();
			return;
		}

		try {
			const recorder = await startSttProviderRecording(providerId, {
				onTranscribing: () => {
					setActiveRecorder(null);
					setVoiceTesting({ id: providerId, type, phase: "transcribing" });
					rerender();
				},
				onResult: (result) => {
					setVoiceTestResults((current) => ({ ...current, [providerId]: result }));
					setVoiceTesting(null);
					rerender();
				},
			});
			setActiveRecorder(recorder);
			setVoiceTesting({ id: providerId, type, phase: "recording" });
		} catch (caught) {
			showToast(humanizeMicError(caught as { name?: string; message?: string }), "error");
			setVoiceTesting(null);
		}
		rerender();
	}

	async function testPersona(personaId: string): Promise<void> {
		const setPhase = (phase: PersonaTestPhase): void => {
			setPersonaTesting((current) => ({ ...current, [personaId]: phase }));
			rerender();
		};
		setPhase("testing");
		const started = await startPersonaTest(personaId, {
			onPlaying: () => setPhase("playing"),
			onEnded: () => setPhase(""),
		});
		if (!started) setPhase("");
	}

	async function setActivePersona(personaId: string | null): Promise<void> {
		await setActiveVoicePersona(personaId);
		await fetchPersonas();
	}

	async function removePersona(personaId: string): Promise<void> {
		await deleteVoicePersona(personaId);
		await fetchPersonas();
	}

	function finishPersonaEditing(): void {
		setPersonaEditing(null);
		void fetchPersonas();
	}

	if (voiceLoading || !connected.value) {
		return (
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Voice</h2>
				<div className="text-xs text-[var(--muted)]">{connected.value ? "Loading\u2026" : "Connecting\u2026"}</div>
			</div>
		);
	}

	const voiceTabs = [
		{ id: "stt", label: "Speech-to-Text" },
		{ id: "tts", label: "Text-to-Speech" },
		{ id: "personas", label: "Voice Personas" },
		{ id: "input", label: "Input Settings" },
	];

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Voice</h2>

			<TabBar tabs={voiceTabs} active={activeTab} onChange={setActiveTab} />

			<div style={{ maxWidth: "700px", display: "flex", flexDirection: "column", gap: "16px" }}>
				{activeTab === "stt" && (
					<div className="flex flex-col gap-3">
						<p className="text-xs text-[var(--muted)] leading-relaxed" style={{ margin: 0 }}>
							STT lets you use the microphone button in chat to record voice input.
						</p>
						{gon.get("stt_enabled") === false && (
							<div className="rounded border border-[var(--border-strong)] bg-[var(--surface2)] px-3 py-2 text-xs text-[var(--muted)]">
								Speech-to-text is disabled in your config (<code>voice.stt.enabled = false</code> in{" "}
								<code>chelix.toml</code>). Provider configuration is shown for reference.
							</div>
						)}
						<div className="flex flex-col gap-2">
							{allProviders.stt.map((prov) => {
								const testState = voiceTesting?.id === prov.id && voiceTesting?.type === "stt" ? voiceTesting : null;
								const testResult = voiceTestResults[prov.id] || null;
								return (
									<VoiceProviderRow
										key={prov.id}
										provider={prov}
										meta={prov}
										type="stt"
										saving={savingProvider === prov.id}
										testState={testState}
										testResult={testResult}
										onToggle={(enabled: boolean) => onToggleProvider(prov, enabled, "stt")}
										onConfigure={() => onConfigureProvider(prov.id, prov)}
										onTest={() => testVoiceProvider(prov.id, "stt")}
									/>
								);
							})}
						</div>
					</div>
				)}

				{activeTab === "tts" && (
					<div className="flex flex-col gap-3">
						<p className="text-xs text-[var(--muted)] leading-relaxed" style={{ margin: 0 }}>
							TTS lets you hear responses as audio. Configure providers and test voices.
						</p>
						{gon.get("tts_enabled") === false && (
							<div className="rounded border border-[var(--border-strong)] bg-[var(--surface2)] px-3 py-2 text-xs text-[var(--muted)]">
								Text-to-speech is disabled in your config (<code>voice.tts.enabled = false</code> in{" "}
								<code>chelix.toml</code>). Provider configuration is shown for reference.
							</div>
						)}
						<div className="flex flex-col gap-2">
							{allProviders.tts.map((prov) => {
								const testState = voiceTesting?.id === prov.id && voiceTesting?.type === "tts" ? voiceTesting : null;
								const testResult = voiceTestResults[prov.id] || null;
								return (
									<VoiceProviderRow
										key={prov.id}
										provider={prov}
										meta={prov}
										type="tts"
										saving={savingProvider === prov.id}
										testState={testState}
										testResult={testResult}
										onToggle={(enabled: boolean) => onToggleProvider(prov, enabled, "tts")}
										onConfigure={() => onConfigureProvider(prov.id, prov)}
										onTest={() => testVoiceProvider(prov.id, "tts")}
										preferred={prov.preferred}
										onSetPreferred={() => {
											sendRpc("tts.setProvider", { provider: prov.id }).then(() => {
												fetchVoiceStatus({ silent: true });
												rerender();
											});
										}}
									/>
								);
							})}
						</div>
					</div>
				)}

				{activeTab === "personas" && (
					<VoicePersonasPanel
						personas={personas}
						testing={personaTesting}
						editingId={personaEditing}
						onTest={(personaId) => void testPersona(personaId)}
						onEdit={setPersonaEditing}
						onSetActive={(personaId) => void setActivePersona(personaId)}
						onRemove={(personaId) => void removePersona(personaId)}
						onCloseEditor={() => setPersonaEditing(null)}
						onSaved={finishPersonaEditing}
					/>
				)}

				{activeTab === "input" && (
					<div className="flex flex-col gap-6">
						<div className="flex flex-col gap-3">
							<h3 className="text-sm font-medium text-[var(--text-strong)]">Push-to-Talk</h3>
							<p className="text-xs text-[var(--muted)] leading-relaxed" style={{ margin: 0 }}>
								Hold a keyboard key to record voice input. Release to send. Function keys (F1–F24) work even when
								focused in an input field.
							</p>
							<div className="flex items-center gap-3">
								<span className="text-xs text-[var(--muted)]">PTT Key:</span>
								<PttKeyPicker
									pttListening={pttListening}
									setPttListening={setPttListening}
									pttKeyValue={pttKeyValue}
									setPttKeyValue={setPttKeyValue}
								/>
							</div>
						</div>

						<div className="flex flex-col gap-3">
							<h3 className="text-sm font-medium text-[var(--text-strong)]">Conversation Mode (VAD)</h3>
							<p className="text-xs text-[var(--muted)] leading-relaxed" style={{ margin: 0 }}>
								Adjust how sensitive the voice activity detection is. Higher values pick up softer speech but may
								trigger on background noise.
							</p>
							<div className="flex items-center gap-3">
								<span className="text-xs text-[var(--muted)]" style={{ minWidth: "80px" }}>
									Sensitivity:
								</span>
								<input
									type="range"
									min="0"
									max="100"
									step="5"
									value={vadSens}
									style={{ flex: 1, maxWidth: "200px", accentColor: "var(--accent)" }}
									onInput={(e) => {
										const val = parseInt(targetValue(e), 10);
										setVadSens(val);
										setVadSensitivity(val);
										rerender();
									}}
								/>
								<span className="text-xs text-[var(--muted)]" style={{ minWidth: "35px", textAlign: "right" }}>
									{vadSens}%
								</span>
							</div>
						</div>
					</div>
				)}
			</div>

			<AddVoiceProviderModal
				unconfiguredProviders={getUnconfiguredProviders()}
				voxtralReqs={voxtralReqs}
				onSaved={() => {
					fetchVoiceStatus();
					voiceShowAddModal.value = false;
					voiceSelectedProvider.value = null;
					voiceSelectedProviderData.value = null;
				}}
			/>
		</div>
	);
}

// Individual provider row with enable toggle

interface VoiceProviderRowProps {
	provider: VoiceProviderData;
	meta: VoiceProviderData;
	type: string;
	saving: boolean;
	testState: VoiceTesting | null;
	testResult: VoiceTestResult | null;
	onToggle: (enabled: boolean) => void;
	onConfigure: () => void;
	onTest: () => void;
	preferred?: boolean;
	onSetPreferred?: () => void;
}

interface VoiceTestButtonState {
	text: string;
	disabled: boolean;
}

function voiceTestButtonState(testState: VoiceTesting | null): VoiceTestButtonState {
	if (testState?.phase === "recording") return { text: "Stop", disabled: false };
	if (testState) return { text: "Testing\u2026", disabled: true };
	return { text: "Test", disabled: false };
}

function voiceKeySourceLabel(source?: string): string {
	if (source === "env") return "(from env)";
	if (source === "llm_provider") return "(from LLM provider)";
	return "";
}

function VoiceProviderIdentity({
	provider,
	meta,
	preferred,
}: Pick<VoiceProviderRowProps, "provider" | "meta" | "preferred">): VNode {
	const keySource = voiceKeySourceLabel(provider.keySource);
	return (
		<>
			<div className="flex items-center gap-2">
				<span className="text-sm text-[var(--text-strong)]">{meta.name}</span>
				{preferred && (
					<span className="text-[10px] px-1.5 py-0.5 rounded bg-[var(--accent)] text-white">preferred</span>
				)}
				{provider.category === "local" && <span className="provider-item-badge">local</span>}
				{keySource && <span className="text-xs text-[var(--muted)]">{keySource}</span>}
			</div>
			<span className="text-xs text-[var(--muted)]">{meta.description}</span>
		</>
	);
}

function VoiceProviderMetadata({ provider, type }: Pick<VoiceProviderRowProps, "provider" | "type">): VNode {
	return (
		<>
			{provider.settingsSummary && (
				<span className="text-xs text-[var(--muted)]">
					{type === "tts" ? "Voice" : "Settings"}: {provider.settingsSummary}
				</span>
			)}
			{provider.binaryPath && <span className="text-xs text-[var(--muted)]">Found at: {provider.binaryPath}</span>}
			{!provider.available && provider.statusMessage && (
				<span className="text-xs text-[var(--muted)]">{provider.statusMessage}</span>
			)}
		</>
	);
}

function VoiceTestProgress({ state, type }: { state: VoiceTesting | null; type: string }): VNode | null {
	if (state?.phase === "recording") {
		return (
			<div className="voice-recording-hint">
				<span className="voice-recording-dot" />
				<span>Speak now, then click Stop when finished</span>
			</div>
		);
	}
	if (state?.phase === "transcribing") return <span className="text-xs text-[var(--muted)]">Transcribing...</span>;
	if (state?.phase === "testing" && type === "tts")
		return <span className="text-xs text-[var(--muted)]">Playing audio...</span>;
	return null;
}

function VoiceTestFeedback({ result }: { result: VoiceTestResult | null }): VNode {
	return (
		<>
			{result?.text && (
				<div className="voice-transcription-result">
					<span className="voice-transcription-label">Transcribed:</span>
					<span className="voice-transcription-text">"{result.text}"</span>
				</div>
			)}
			{result?.success === true && (
				<div className="voice-success-result">
					<span className="icon icon-md icon-check-circle" />
					<span>Audio played successfully</span>
				</div>
			)}
			{result?.error && (
				<div className="voice-error-result">
					<span className="icon icon-md icon-x-circle" />
					<span>{result.error}</span>
				</div>
			)}
		</>
	);
}

function VoiceProviderActions(props: VoiceProviderRowProps): VNode {
	const button = voiceTestButtonState(props.testState);
	return (
		<div className="flex items-center gap-2">
			{props.onSetPreferred && props.provider.enabled && !props.preferred && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary text-xs !py-1 !px-2"
					onClick={props.onSetPreferred}
					title="Set as preferred TTS provider"
				>
					📌
				</button>
			)}
			<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={props.onConfigure}>
				Configure
			</button>
			{props.provider.available && props.provider.enabled && (
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm"
					onClick={props.onTest}
					disabled={button.disabled}
					title={props.type === "tts" ? "Test voice output" : "Test voice input"}
				>
					{button.text}
				</button>
			)}
			<VoiceProviderToggle {...props} />
		</div>
	);
}

function VoiceProviderToggle({
	provider,
	saving,
	onToggle,
}: Pick<VoiceProviderRowProps, "provider" | "saving" | "onToggle">): VNode | null {
	if (provider.available) {
		return (
			<label className="toggle-switch">
				<input
					type="checkbox"
					checked={provider.enabled}
					disabled={saving}
					onChange={(event) => onToggle(targetChecked(event))}
				/>
				<span className="toggle-slider" />
			</label>
		);
	}
	return provider.category === "local" ? <span className="text-xs text-[var(--muted)]">Install required</span> : null;
}

function VoiceProviderRow(props: VoiceProviderRowProps): VNode {
	return (
		<div className="provider-card flex items-center gap-3 py-2.5 px-3.5 rounded-lg">
			<div className="flex-1 flex flex-col gap-0.5">
				<VoiceProviderIdentity provider={props.provider} meta={props.meta} preferred={props.preferred} />
				<VoiceProviderMetadata provider={props.provider} type={props.type} />
				<VoiceTestProgress state={props.testState} type={props.type} />
				<VoiceTestFeedback result={props.testResult} />
			</div>
			<VoiceProviderActions {...props} />
		</div>
	);
}
