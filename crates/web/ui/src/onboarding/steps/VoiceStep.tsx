// ── Voice step (TTS/STT configuration) ───────────────────────

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { get as getGon } from "../../gon";
import { t } from "../../i18n";
import { activeSessionKey } from "../../state";
import { fetchPhrase } from "../../tts-phrases";
import { targetValue } from "../../typed-events";
import {
	decodeBase64Safe,
	fetchVoiceProviders,
	saveVoiceKey,
	saveVoiceSettings,
	testTts,
	toggleVoiceProvider,
	transcribeAudio,
	VOICE_COUNTERPART_IDS,
} from "../../voice-utils";
import { ErrorPanel, ensureWsConnected } from "../shared";
import type { IdentityInfo } from "../types";

// ── Constants ───────────────────────────────────────────────

const WS_RETRY_LIMIT = 75;
const WS_RETRY_DELAY_MS = 200;

// ── Types ───────────────────────────────────────────────────

interface VoiceProvider {
	id: string;
	name: string;
	description?: string;
	category: string;
	available: boolean;
	enabled: boolean;
	keySource?: string;
	keyUrl?: string;
	keyUrlLabel?: string;
	keyPlaceholder?: string;
	hint?: string;
	capabilities?: { baseUrl?: boolean };
	settings?: { baseUrl?: string };
	[key: string]: unknown;
}

interface VoiceProviders {
	tts: VoiceProvider[];
	stt: VoiceProvider[];
}

type VoiceType = "stt" | "tts";
type VoiceTestPhase = "testing" | "recording" | "transcribing";

interface VoiceTesting {
	id: string;
	type: VoiceType;
	phase: VoiceTestPhase;
}

interface VoiceTestResult {
	success?: boolean;
	text?: string | null;
	error?: string | null;
}

// ── OnboardingVoiceRow ──────────────────────────────────────

interface OnboardingVoiceRowProps {
	provider: VoiceProvider;
	type: VoiceType;
	configuring: string | null;
	apiKey: string;
	setApiKey: (value: string) => void;
	baseUrl: string;
	setBaseUrl: (value: string) => void;
	saving: boolean;
	error: string | null;
	onSaveKey: (event: Event) => void;
	onStartConfigure: (id: string) => void;
	onCancelConfigure: () => void;
	onTest: () => void;
	voiceTesting: VoiceTesting | null;
	voiceTestResult: VoiceTestResult | null;
}

interface VoiceTestButtonView {
	text: string;
	disabled: boolean;
}

function voiceKeySourceLabel(provider: VoiceProvider): string {
	if (provider.keySource === "env") return "(from env)";
	return provider.keySource === "llm_provider" ? "(from LLM provider)" : "";
}

function voiceTestButtonView(testState: VoiceTesting | null): VoiceTestButtonView {
	if (!testState) return { text: "Test", disabled: false };
	if (testState.phase === "recording") return { text: "Stop", disabled: false };
	return { text: "Testing\u2026", disabled: true };
}

interface VoiceProviderSummaryProps {
	provider: VoiceProvider;
	type: VoiceType;
	isConfiguring: boolean;
	testState: VoiceTesting | null;
	onStartConfigure: (id: string) => void;
	onTest: () => void;
}

function VoiceProviderSummary(props: VoiceProviderSummaryProps): VNode {
	const keySourceLabel = voiceKeySourceLabel(props.provider);
	const testButton = voiceTestButtonView(props.testState);
	return (
		<div className="flex items-center gap-3">
			<div className="flex-1 min-w-0 flex flex-col gap-0.5">
				<div className="flex items-center gap-2 flex-wrap">
					<span className="text-sm font-medium text-[var(--text-strong)]">{props.provider.name}</span>
					<span className={`provider-item-badge ${props.provider.available ? "configured" : "needs-key"}`}>
						{props.provider.available ? "configured" : "needs key"}
					</span>
					{keySourceLabel && <span className="text-xs text-[var(--muted)]">{keySourceLabel}</span>}
				</div>
				{props.provider.description && (
					<span className="text-xs text-[var(--muted)]">
						{props.provider.description}
						{!props.isConfiguring && props.provider.keyUrl && (
							<>
								{" \u2014 "}get your key at{" "}
								<a
									href={props.provider.keyUrl}
									target="_blank"
									rel="noopener"
									className="text-[var(--accent)] underline"
								>
									{props.provider.keyUrlLabel || props.provider.keyUrl}
								</a>
							</>
						)}
					</span>
				)}
			</div>
			<div className="shrink-0 flex items-center gap-2">
				{!props.isConfiguring && (
					<button
						type="button"
						className="provider-btn provider-btn-secondary provider-btn-sm"
						onClick={() => props.onStartConfigure(props.provider.id)}
					>
						Configure
					</button>
				)}
				{props.provider.available && (
					<button
						type="button"
						className="provider-btn provider-btn-secondary provider-btn-sm"
						onClick={props.onTest}
						disabled={testButton.disabled}
						title={props.type === "tts" ? "Test voice output" : "Test voice input"}
					>
						{testButton.text}
					</button>
				)}
			</div>
		</div>
	);
}

function VoiceTestFeedback({
	type,
	testState,
	result,
}: {
	type: VoiceType;
	testState: VoiceTesting | null;
	result: VoiceTestResult | null;
}): VNode {
	return (
		<>
			{testState?.phase === "recording" && (
				<div className="voice-recording-hint mt-2">
					<span className="voice-recording-dot" />
					<span>Speak now, then click Stop when finished</span>
				</div>
			)}
			{testState?.phase === "transcribing" && (
				<span className="text-xs text-[var(--muted)] mt-1 block">Transcribing&hellip;</span>
			)}
			{testState?.phase === "testing" && type === "tts" && (
				<span className="text-xs text-[var(--muted)] mt-1 block">Playing audio&hellip;</span>
			)}
			{result?.text && (
				<div className="voice-transcription-result mt-2">
					<span className="voice-transcription-label">Transcribed:</span>
					<span className="voice-transcription-text">"{result.text}"</span>
				</div>
			)}
			{result?.success === true && (
				<div className="voice-success-result mt-2">
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

interface VoiceConfigurationFormProps {
	provider: VoiceProvider;
	apiKey: string;
	setApiKey: (value: string) => void;
	baseUrl: string;
	setBaseUrl: (value: string) => void;
	saving: boolean;
	error: string | null;
	onSaveKey: (event: Event) => void;
	onCancel: () => void;
}

function VoiceConfigurationForm(props: VoiceConfigurationFormProps): VNode {
	const keyInputRef = useRef<HTMLInputElement>(null);
	useEffect(() => keyInputRef.current?.focus(), []);
	return (
		<form onSubmit={props.onSaveKey} className="flex flex-col gap-2 mt-3 border-t border-[var(--border)] pt-3">
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">API Key</span>
				<input
					type="password"
					className="provider-key-input w-full"
					ref={keyInputRef}
					value={props.apiKey}
					onInput={(event) => props.setApiKey(targetValue(event))}
					placeholder={props.provider.keyPlaceholder || "API key"}
				/>
			</label>
			{props.provider.capabilities?.baseUrl === true && (
				<div>
					<label>
						<span className="text-xs text-[var(--muted)] mb-1 block">Base URL</span>
						<input
							type="text"
							className="provider-key-input w-full"
							data-field="baseUrl"
							value={props.baseUrl}
							onInput={(event) => props.setBaseUrl(targetValue(event))}
							placeholder="http://localhost:8000/v1"
						/>
					</label>
					<div className="text-xs text-[var(--muted)] mt-1">
						Use this for a local or OpenAI-compatible server. Leave the API key blank if the endpoint does not require
						one.
					</div>
				</div>
			)}
			{props.provider.keyUrl && (
				<div className="text-xs text-[var(--muted)]">
					Get your key at{" "}
					<a href={props.provider.keyUrl} target="_blank" rel="noopener" className="text-[var(--accent)] underline">
						{props.provider.keyUrlLabel || props.provider.keyUrl}
					</a>
				</div>
			)}
			{props.provider.hint && <div className="text-xs text-[var(--accent)]">{props.provider.hint}</div>}
			{props.error && <ErrorPanel message={props.error} />}
			<div className="flex items-center gap-2 mt-1">
				<button type="submit" className="provider-btn provider-btn-sm" disabled={props.saving}>
					{props.saving ? "Saving\u2026" : "Save"}
				</button>
				<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={props.onCancel}>
					Cancel
				</button>
			</div>
		</form>
	);
}

function OnboardingVoiceRow(props: OnboardingVoiceRowProps): VNode {
	const isConfiguring = props.configuring === props.provider.id;
	const testState =
		props.voiceTesting?.id === props.provider.id && props.voiceTesting.type === props.type ? props.voiceTesting : null;
	return (
		<div className="rounded-md border border-[var(--border)] bg-[var(--surface)] p-3">
			<VoiceProviderSummary
				provider={props.provider}
				type={props.type}
				isConfiguring={isConfiguring}
				testState={testState}
				onStartConfigure={props.onStartConfigure}
				onTest={props.onTest}
			/>
			<VoiceTestFeedback type={props.type} testState={testState} result={props.voiceTestResult} />
			{isConfiguring && (
				<VoiceConfigurationForm
					provider={props.provider}
					apiKey={props.apiKey}
					setApiKey={props.setApiKey}
					baseUrl={props.baseUrl}
					setBaseUrl={props.setBaseUrl}
					saving={props.saving}
					error={props.error}
					onSaveKey={props.onSaveKey}
					onCancel={props.onCancelConfigure}
				/>
			)}
		</div>
	);
}

interface VoiceSaveResponse {
	ok?: boolean;
	error?: { message?: string };
}

interface VoiceSaveRequest {
	providerId: string;
	apiKey: string;
	baseUrl?: string;
	providers: VoiceProviders;
}

function matchingVoiceProvider(providers: VoiceProvider[], providerId: string): VoiceProvider | undefined {
	const counterpartId = VOICE_COUNTERPART_IDS[providerId];
	return providers.find((provider) => provider.id === providerId || provider.id === counterpartId);
}

async function enableSavedVoiceProvider(providerId: string, providers: VoiceProviders): Promise<void> {
	const requests: Promise<unknown>[] = [];
	const sttProvider = matchingVoiceProvider(providers.stt, providerId);
	const ttsProvider = matchingVoiceProvider(providers.tts, providerId);
	if (sttProvider) requests.push(toggleVoiceProvider(sttProvider.id, true, "stt"));
	if (ttsProvider) requests.push(toggleVoiceProvider(ttsProvider.id, true, "tts"));
	await Promise.all(requests);
}

async function saveAndEnableVoiceProvider(request: VoiceSaveRequest): Promise<VoiceSaveResponse> {
	const response = request.apiKey
		? ((await saveVoiceKey(request.providerId, request.apiKey, { baseUrl: request.baseUrl })) as VoiceSaveResponse)
		: ((await saveVoiceSettings(
				request.providerId,
				request.baseUrl === undefined ? undefined : { baseUrl: request.baseUrl },
			)) as VoiceSaveResponse);
	if (response.ok) await enableSavedVoiceProvider(request.providerId, request.providers);
	return response;
}

interface VoiceToggleResponse {
	ok?: boolean;
	error?: { message?: string };
}

interface VoiceEnableResult {
	changed: boolean;
	error: string | null;
}

async function enableVoiceProviderForTest(
	providerId: string,
	type: VoiceType,
	providers: VoiceProviders,
): Promise<VoiceEnableResult> {
	const providerList = type === "stt" ? providers.stt : providers.tts;
	const provider = providerList.find((candidate) => candidate.id === providerId);
	if (!(provider?.available && !provider.enabled)) return { changed: false, error: null };
	const response = (await toggleVoiceProvider(providerId, true, type)) as VoiceToggleResponse;
	if (!response.ok) {
		return { changed: false, error: response.error?.message || "Failed to enable provider" };
	}
	const counterpartType: VoiceType = type === "stt" ? "tts" : "stt";
	const counterpartList = counterpartType === "stt" ? providers.stt : providers.tts;
	const counterpartId = VOICE_COUNTERPART_IDS[providerId] || providerId;
	const counterpart = counterpartList.find((candidate) => candidate.id === counterpartId);
	if (counterpart?.available && !counterpart.enabled) {
		await toggleVoiceProvider(counterpartId, true, counterpartType);
	}
	return { changed: true, error: null };
}

function playVoiceTestAudio(payload: { audio: string; mimeType?: string; content_type?: string }): void {
	const bytes = decodeBase64Safe(payload.audio);
	const audioMime = payload.mimeType || payload.content_type || "audio/mpeg";
	const url = URL.createObjectURL(new Blob([bytes.buffer as ArrayBuffer], { type: audioMime }));
	const audio = new Audio(url);
	audio.onerror = (event) => {
		console.error("[TTS] audio element error:", audio.error?.message || event);
		URL.revokeObjectURL(url);
	};
	audio.onended = () => URL.revokeObjectURL(url);
	audio.play().catch((error) => console.error("[TTS] play() failed:", error));
}

async function runTtsVoiceTest(providerId: string): Promise<VoiceTestResult> {
	try {
		const identity = getGon("identity") as IdentityInfo | null;
		const text = await fetchPhrase("onboarding", identity?.user_name || "friend", identity?.name || "Chelix");
		const response = (await testTts(text, providerId)) as {
			ok?: boolean;
			payload?: { audio?: string; mimeType?: string; content_type?: string };
			error?: { message?: string };
		};
		if (!(response.ok && response.payload?.audio)) {
			return { success: false, error: response.error?.message || "TTS test failed" };
		}
		playVoiceTestAudio({ ...response.payload, audio: response.payload.audio });
		return { success: true, error: null };
	} catch (error) {
		return { success: false, error: (error as Error).message || "TTS test failed" };
	}
}

async function failedTranscriptionResult(response: Response): Promise<VoiceTestResult> {
	const body = await response.text();
	console.error("[STT] upload failed: status=%d body=%s", response.status, body);
	let message = "STT test failed";
	try {
		message = (JSON.parse(body) as { error?: string }).error || message;
	} catch {
		// The HTTP status remains actionable when the server response is not JSON.
	}
	return { text: null, error: `${message} (HTTP ${response.status})` };
}

async function transcriptionResult(providerId: string, audio: Blob): Promise<VoiceTestResult> {
	try {
		const response = await transcribeAudio(activeSessionKey, providerId, audio);
		if (!response.ok) return failedTranscriptionResult(response);
		const result = (await response.json()) as {
			ok?: boolean;
			transcription?: { text?: string };
			transcriptionError?: string;
			error?: string;
		};
		if (!(result.ok && typeof result.transcription?.text === "string")) {
			return { text: null, error: result.transcriptionError || result.error || "STT test failed" };
		}
		const text = result.transcription.text.trim();
		return { text: text || null, error: text ? null : "No speech detected" };
	} catch (error) {
		return { text: null, error: (error as Error).message || "STT test failed" };
	}
}

interface VoiceRecordingCallbacks {
	onRecorder: (recorder: MediaRecorder | null) => void;
	onPhase: (phase: VoiceTestPhase) => void;
	onResult: (result: VoiceTestResult) => void;
}

async function finishVoiceRecording(
	providerId: string,
	recorder: MediaRecorder,
	stream: MediaStream,
	chunks: Blob[],
	fallbackMimeType: string,
	callbacks: VoiceRecordingCallbacks,
): Promise<void> {
	callbacks.onRecorder(null);
	for (const track of stream.getTracks()) track.stop();
	callbacks.onPhase("transcribing");
	const audio = new Blob(chunks, { type: recorder.mimeType || fallbackMimeType });
	callbacks.onResult(await transcriptionResult(providerId, audio));
}

async function startVoiceRecording(providerId: string, callbacks: VoiceRecordingCallbacks): Promise<MediaRecorder> {
	const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
	const mimeType = MediaRecorder.isTypeSupported("audio/webm;codecs=opus") ? "audio/webm;codecs=opus" : "audio/webm";
	const recorder = new MediaRecorder(stream, { mimeType });
	const chunks: Blob[] = [];
	recorder.ondataavailable = (event) => {
		if (event.data.size > 0) chunks.push(event.data);
	};
	recorder.onstop = () => {
		void finishVoiceRecording(providerId, recorder, stream, chunks, mimeType, callbacks);
	};
	recorder.start();
	callbacks.onRecorder(recorder);
	callbacks.onPhase("recording");
	return recorder;
}

function microphoneErrorMessage(error: unknown): string {
	const domError = error as DOMException;
	if (domError.name === "NotAllowedError") return "Microphone permission denied";
	if (domError.name === "NotFoundError") return "No microphone found";
	return domError.message || "STT test failed";
}

// ── VoiceStep ───────────────────────────────────────────────

export function VoiceStep({ onNext, onBack }: { onNext: () => void; onBack: () => void }): VNode {
	const [loading, setLoading] = useState(true);
	const [allProviders, setAllProviders] = useState<VoiceProviders>({ tts: [], stt: [] });
	const [configuring, setConfiguring] = useState<string | null>(null);
	const [apiKey, setApiKey] = useState("");
	const [baseUrl, setBaseUrl] = useState("");
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [voiceTesting, setVoiceTesting] = useState<VoiceTesting | null>(null);
	const [voiceTestResults, setVoiceTestResults] = useState<Record<string, VoiceTestResult>>({});
	const [activeRecorder, setActiveRecorder] = useState<MediaRecorder | null>(null);
	const [enableSaving, setEnableSaving] = useState(false);

	function fetchProviders(): Promise<unknown> {
		return (fetchVoiceProviders() as Promise<{ ok?: boolean; payload?: VoiceProviders }>).then((res) => {
			if (res?.ok) {
				setAllProviders(res.payload || { tts: [], stt: [] });
			}
			return res;
		});
	}

	useEffect(() => {
		let cancelled = false;
		let attempts = 0;

		function load(): void {
			if (cancelled) return;
			(
				fetchVoiceProviders() as Promise<{
					ok?: boolean;
					payload?: VoiceProviders;
					error?: { code?: string; message?: string };
				}>
			).then((res) => {
				if (cancelled) return;
				if (res?.ok) {
					setAllProviders(res.payload || { tts: [], stt: [] });
					setLoading(false);
					return;
				}
				if (
					(res?.error?.code === "UNAVAILABLE" || res?.error?.message === "WebSocket not connected") &&
					attempts < WS_RETRY_LIMIT
				) {
					attempts += 1;
					ensureWsConnected();
					window.setTimeout(load, WS_RETRY_DELAY_MS);
					return;
				}
				// Voice not compiled -> skip
				onNext();
			});
		}

		load();
		return () => {
			cancelled = true;
		};
	}, []);

	// Cloud providers only (filter out local for onboarding)
	const cloudStt = allProviders.stt.filter((p) => p.category === "cloud");
	const cloudTts = allProviders.tts.filter((p) => p.category === "cloud");

	// Auto-detected: available via LLM provider key, not yet enabled.
	const autoDetected = [...allProviders.stt, ...allProviders.tts].filter(
		(p) => p.available && p.keySource === "llm_provider" && !p.enabled && p.category === "cloud",
	);
	const hasAutoDetected = autoDetected.length > 0;

	function enableAutoDetected(): void {
		setEnableSaving(true);
		setError(null);
		const firstStt = allProviders.stt.find((p) => p.available && p.keySource === "llm_provider" && !p.enabled);
		const firstTts = allProviders.tts.find((p) => p.available && p.keySource === "llm_provider" && !p.enabled);
		const toggles: Promise<unknown>[] = [];
		if (firstStt) toggles.push(toggleVoiceProvider(firstStt.id, true, "stt"));
		if (firstTts) toggles.push(toggleVoiceProvider(firstTts.id, true, "tts"));
		if (toggles.length === 0) {
			setEnableSaving(false);
			return;
		}
		Promise.all(toggles).then((results) => {
			setEnableSaving(false);
			const failed = (results as Array<{ ok?: boolean; error?: { message?: string } }>).find((r) => !r?.ok);
			if (failed) {
				setError(failed?.error?.message || "Failed to enable voice provider");
				return;
			}
			fetchProviders();
		});
	}

	function onStartConfigure(providerId: string): void {
		const provider = [...allProviders.stt, ...allProviders.tts].find((candidate) => candidate.id === providerId);
		setConfiguring(providerId);
		setApiKey("");
		setBaseUrl(provider?.settings?.baseUrl || "");
		setError(null);
	}

	function onCancelConfigure(): void {
		setConfiguring(null);
		setApiKey("");
		setBaseUrl("");
		setError(null);
	}

	function onSaveKey(event: Event): void {
		event.preventDefault();
		if (!configuring) return;
		const provider = [...allProviders.stt, ...allProviders.tts].find((candidate) => candidate.id === configuring);
		const trimmedApiKey = apiKey.trim();
		const trimmedBaseUrl = baseUrl.trim();
		const hadBaseUrl = typeof provider?.settings?.baseUrl === "string" && provider.settings.baseUrl.trim().length > 0;
		const shouldSaveBaseUrl = provider?.capabilities?.baseUrl === true && (trimmedBaseUrl.length > 0 || hadBaseUrl);
		if (!(trimmedApiKey || shouldSaveBaseUrl)) {
			setError("API key or base URL is required.");
			return;
		}
		setError(null);
		setSaving(true);
		saveAndEnableVoiceProvider({
			providerId: configuring,
			apiKey: trimmedApiKey,
			baseUrl: shouldSaveBaseUrl ? trimmedBaseUrl : undefined,
			providers: allProviders,
		}).then((response) => {
			setSaving(false);
			if (!response.ok) {
				setError(response.error?.message || "Failed to save");
				return;
			}
			setConfiguring(null);
			setApiKey("");
			setBaseUrl("");
			void fetchProviders();
		});
	}

	function updateVoiceTestResult(providerId: string, result: VoiceTestResult): void {
		setVoiceTestResults((previous) => ({ ...previous, [providerId]: result }));
	}

	function stopActiveRecording(): void {
		activeRecorder?.stop();
	}

	async function testVoiceProvider(providerId: string, type: VoiceType): Promise<void> {
		if (voiceTesting?.id === providerId && voiceTesting.type === "stt" && voiceTesting.phase === "recording") {
			stopActiveRecording();
			return;
		}
		setError(null);
		setVoiceTesting({ id: providerId, type, phase: "testing" });
		const enableResult = await enableVoiceProviderForTest(providerId, type, allProviders);
		if (enableResult.error) {
			updateVoiceTestResult(providerId, { success: false, error: enableResult.error });
			setVoiceTesting(null);
			return;
		}
		if (enableResult.changed) void fetchProviders();
		if (type === "tts") {
			updateVoiceTestResult(providerId, await runTtsVoiceTest(providerId));
			setVoiceTesting(null);
			return;
		}
		try {
			await startVoiceRecording(providerId, {
				onRecorder: setActiveRecorder,
				onPhase: (phase) => setVoiceTesting({ id: providerId, type: "stt", phase }),
				onResult: (result) => {
					updateVoiceTestResult(providerId, result);
					setVoiceTesting(null);
				},
			});
		} catch (recordingError) {
			setError(microphoneErrorMessage(recordingError));
			setVoiceTesting(null);
		}
	}

	// ── Render ────────────────────────────────────────────────

	if (loading) {
		return <div className="text-sm text-[var(--muted)]">Checking voice providers&hellip;</div>;
	}

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Voice (optional)</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Enable voice input (speech-to-text) and output (text-to-speech) for your agent. You can configure this later in
				Settings.
			</p>

			{hasAutoDetected ? (
				<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 flex flex-col gap-2">
					<div className="text-xs text-[var(--muted)]">Auto-detected from your LLM provider</div>
					<div className="flex flex-wrap gap-2">
						{autoDetected.map((p) => (
							<span key={p.id} className="provider-item-badge configured">
								{p.name}
							</span>
						))}
					</div>
					<button
						type="button"
						className="provider-btn self-start"
						disabled={enableSaving}
						onClick={enableAutoDetected}
					>
						{enableSaving ? "Enabling\u2026" : "Enable voice"}
					</button>
				</div>
			) : null}

			{cloudStt.length > 0 ? (
				<div>
					<h3 className="text-sm font-medium text-[var(--text-strong)] mb-2">Speech-to-Text</h3>
					<div className="flex flex-col gap-2">
						{cloudStt.map((prov) => (
							<OnboardingVoiceRow
								key={prov.id}
								provider={prov}
								type="stt"
								configuring={configuring}
								apiKey={apiKey}
								setApiKey={setApiKey}
								baseUrl={baseUrl}
								setBaseUrl={setBaseUrl}
								saving={saving}
								error={configuring === prov.id ? error : null}
								onSaveKey={onSaveKey}
								onStartConfigure={onStartConfigure}
								onCancelConfigure={onCancelConfigure}
								onTest={() => testVoiceProvider(prov.id, "stt")}
								voiceTesting={voiceTesting}
								voiceTestResult={voiceTestResults[prov.id] || null}
							/>
						))}
					</div>
				</div>
			) : null}

			{cloudTts.length > 0 ? (
				<div>
					<h3 className="text-sm font-medium text-[var(--text-strong)] mb-2">Text-to-Speech</h3>
					<div className="flex flex-col gap-2">
						{cloudTts.map((prov) => (
							<OnboardingVoiceRow
								key={prov.id}
								provider={prov}
								type="tts"
								configuring={configuring}
								apiKey={apiKey}
								setApiKey={setApiKey}
								baseUrl={baseUrl}
								setBaseUrl={setBaseUrl}
								saving={saving}
								error={configuring === prov.id ? error : null}
								onSaveKey={onSaveKey}
								onStartConfigure={onStartConfigure}
								onCancelConfigure={onCancelConfigure}
								onTest={() => testVoiceProvider(prov.id, "tts")}
								voiceTesting={voiceTesting}
								voiceTestResult={voiceTestResults[prov.id] || null}
							/>
						))}
					</div>
				</div>
			) : null}

			{error && !configuring ? <ErrorPanel message={error} /> : null}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
					{t("common:actions.back")}
				</button>
				<button type="button" className="provider-btn" onClick={onNext}>
					{t("common:actions.continue")}
				</button>
				<button
					type="button"
					className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none underline"
					onClick={onNext}
				>
					{t("common:actions.skip")}
				</button>
			</div>
		</div>
	);
}
