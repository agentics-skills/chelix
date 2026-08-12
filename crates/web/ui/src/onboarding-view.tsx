// ── Onboarding wizard ──────────────────────────────────────
//
// Multi-step setup page shown to first-time users.
// Steps: Auth (conditional) → Import (conditional) → Provider →
// Voice (conditional) → Skills → Channel → Identity → Summary
// No new Rust code — all existing RPC methods and REST endpoints.

import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { fetchChannelStatus } from "./channel-utils";
import { get as getGon, refresh as refreshGon } from "./gon";
import { sendRpc } from "./helpers";
import { t } from "./i18n";
// ── Sub-module imports ──────────────────────────────────────
import { ensureWsConnected, preferredChatPath } from "./onboarding/shared";
import { AuthStep } from "./onboarding/steps/AuthStep";
import { ChannelStep } from "./onboarding/steps/ChannelStep";
import { IdentityStep } from "./onboarding/steps/IdentityStep";
import { ImportStep } from "./onboarding/steps/ImportStep";
import { ProviderStep } from "./onboarding/steps/ProviderStep";
import { SkillsStep } from "./onboarding/steps/SkillsStep";
import { VoiceStep } from "./onboarding/steps/VoiceStep";
import type { IdentityInfo } from "./onboarding/types";
import type { SandboxGonInfo } from "./types/gon";
import { fetchVoiceProviders } from "./voice-utils";

// ── Step indicator ──────────────────────────────────────────

interface StepIndicatorProps {
	steps: string[];
	current: number;
}

function StepIndicator({ steps, current }: StepIndicatorProps): VNode {
	const ref = useRef<HTMLDivElement>(null);
	useEffect(() => {
		if (!ref.current) return;
		const active = ref.current.querySelector(".onboarding-step.active");
		if (active) active.scrollIntoView({ inline: "center", block: "nearest", behavior: "smooth" });
	}, [current]);
	return (
		<div className="onboarding-steps" ref={ref}>
			{steps.map((label, i) => {
				const state = i < current ? "completed" : i === current ? "active" : "";
				const isLast = i === steps.length - 1;
				return (
					<>
						<StepDot key={i} index={i} label={label} state={state} />
						{!isLast && <div className={`onboarding-step-line ${i < current ? "completed" : ""}`} />}
					</>
				);
			})}
		</div>
	);
}

function StepDot({ index, label, state }: { index: number; label: string; state: string }): VNode {
	return (
		<div className={`onboarding-step ${state}`}>
			<div className={`onboarding-step-dot ${state}`}>
				{state === "completed" ? <span className="icon icon-md icon-checkmark" /> : index + 1}
			</div>
			<div className="onboarding-step-label">{label}</div>
		</div>
	);
}

// ── Summary step helpers ─────────────────────────────────────

const LOW_MEMORY_THRESHOLD = 2 * 1024 * 1024 * 1024;

function formatMemBytes(bytes: number | null | undefined): string {
	if (bytes == null) return "?";
	const gb = bytes / (1024 * 1024 * 1024);
	return `${gb.toFixed(1)} GB`;
}

function CheckIcon(): VNode {
	return <span className="icon icon-check-circle shrink-0" style="color:var(--ok)" />;
}

function WarnIcon(): VNode {
	return <span className="icon icon-warn-triangle shrink-0" style="color:var(--warn)" />;
}

function ErrorIcon(): VNode {
	return <span className="icon icon-x-circle shrink-0" style="color:var(--error)" />;
}

function InfoIcon(): VNode {
	return <span className="icon icon-info-circle shrink-0" style="color:var(--muted)" />;
}

function SummaryRow({
	icon,
	label,
	children,
}: {
	icon: VNode;
	label: string;
	children: preact.ComponentChildren;
}): VNode {
	return (
		<div className="rounded-md border border-[var(--border)] bg-[var(--surface)] p-3 flex gap-3 items-start">
			<div className="mt-0.5">{icon}</div>
			<div className="flex-1 min-w-0">
				<div className="text-sm font-medium text-[var(--text-strong)]">{label}</div>
				<div className="text-xs text-[var(--muted)] mt-1">{children}</div>
			</div>
		</div>
	);
}

// ── Summary step types ──────────────────────────────────────

interface SummaryProvider {
	name: string;
	displayName: string;
	configured: boolean;
}

interface SummaryChannel {
	type: string;
	account_id: string;
	name?: string;
	status: string;
}

interface SummaryVoiceProvider {
	name: string;
	enabled: boolean;
}

interface SummaryVoice {
	tts: SummaryVoiceProvider[];
	stt: SummaryVoiceProvider[];
}

interface SummarySkills {
	enabledCategories: number;
	totalCategories: number;
	enabledSkills: number;
	totalSkills: number;
}

interface SummaryData {
	identity: IdentityInfo | null;
	mem: { total?: number; available?: number } | null;
	update: { available?: boolean; latest_version?: string; release_url?: string } | null;
	voiceEnabled: boolean;
	providers: SummaryProvider[];
	channels: SummaryChannel[];
	voice: SummaryVoice | null;
	sandbox: SandboxGonInfo | null;
	skills: SummarySkills | null;
}

interface SummarySkillsCategory {
	name: string;
	count: number;
	enabled: boolean;
}

interface SummaryLoadResponses {
	providers: { ok?: boolean; payload?: SummaryProvider[] } | null;
	channels: { ok?: boolean; payload?: { channels?: SummaryChannel[] } } | null;
	voice: { ok?: boolean; payload?: SummaryVoice } | null;
	bootstrap: { sandbox?: SandboxGonInfo } | null;
	skills: {
		ok?: boolean;
		payload?: { categories?: SummarySkillsCategory[]; total_skills?: number };
	} | null;
}

function summarySkills(response: SummaryLoadResponses["skills"]): SummarySkills | null {
	const categories = response?.ok ? response.payload?.categories || [] : [];
	if (!categories.length) return null;
	const enabled = categories.filter((category) => category.enabled);
	return {
		enabledCategories: enabled.length,
		totalCategories: categories.length,
		enabledSkills: enabled.reduce((sum, category) => sum + category.count, 0),
		totalSkills: response?.payload?.total_skills || 0,
	};
}

async function fetchSummaryResponses(voiceEnabled: boolean): Promise<SummaryLoadResponses> {
	const [providers, channels, voice, bootstrap, skills] = await Promise.all([
		(sendRpc("providers.available", {}) as Promise<SummaryLoadResponses["providers"]>).catch(() => null),
		(fetchChannelStatus() as Promise<SummaryLoadResponses["channels"]>).catch(() => null),
		voiceEnabled
			? (fetchVoiceProviders() as Promise<SummaryLoadResponses["voice"]>).catch(() => null)
			: Promise.resolve(null),
		fetch(
			"/api/bootstrap?include_channels=false&include_sessions=false&include_models=false&include_projects=false&include_counts=false&include_identity=false",
		)
			.then((response) => (response.ok ? (response.json() as Promise<SummaryLoadResponses["bootstrap"]>) : null))
			.catch(() => null),
		(sendRpc("skills.bundled.categories", {}) as Promise<SummaryLoadResponses["skills"]>).catch(() => null),
	]);
	return { providers, channels, voice, bootstrap, skills };
}

async function fetchSummaryData(): Promise<SummaryData> {
	await refreshGon();
	const voiceEnabled = getGon("voice_enabled") === true;
	const responses = await fetchSummaryResponses(voiceEnabled);
	return {
		identity: getGon("identity") as IdentityInfo | null,
		mem: getGon("mem") as { total?: number; available?: number } | null,
		update: getGon("update") as SummaryData["update"],
		voiceEnabled,
		providers: responses.providers?.ok ? responses.providers.payload || [] : [],
		channels: responses.channels?.ok ? responses.channels.payload?.channels || [] : [],
		voice: responses.voice?.ok ? responses.voice.payload || { tts: [], stt: [] } : null,
		sandbox: responses.bootstrap?.sandbox || null,
		skills: summarySkills(responses.skills),
	};
}

function IdentitySummary({ identity }: { identity: IdentityInfo | null }): VNode {
	const configured = !!(identity?.user_name && identity.name);
	return (
		<SummaryRow icon={configured ? <CheckIcon /> : <WarnIcon />} label="Identity">
			{configured ? (
				<>
					You: <span className="font-medium text-[var(--text)]">{identity.user_name}</span> Agent:{" "}
					<span className="font-medium text-[var(--text)]">
						{identity.emoji || ""} {identity.name}
					</span>
				</>
			) : (
				<span className="text-[var(--warn)]">Identity not fully configured</span>
			)}
		</SummaryRow>
	);
}

function ProvidersSummary({
	providers,
	activeModel,
}: {
	providers: SummaryProvider[];
	activeModel: string | null;
}): VNode {
	const configured = providers.filter((provider) => provider.configured);
	return (
		<SummaryRow icon={configured.length ? <CheckIcon /> : <ErrorIcon />} label="LLMs">
			{configured.length ? (
				<div className="flex flex-col gap-1">
					<div className="flex flex-wrap gap-1">
						{configured.map((provider) => (
							<span key={provider.name} className="provider-item-badge configured">
								{provider.displayName}
							</span>
						))}
					</div>
					{activeModel && (
						<div>
							Active model: <span className="font-mono font-medium text-[var(--text)]">{activeModel}</span>
						</div>
					)}
				</div>
			) : (
				<span className="text-[var(--error)]">No LLM providers configured</span>
			)}
		</SummaryRow>
	);
}

function channelSummaryIcon(channels: SummaryChannel[]): VNode {
	if (!channels.length) return <InfoIcon />;
	if (channels.some((channel) => channel.status === "error")) return <ErrorIcon />;
	if (channels.some((channel) => channel.status === "disconnected")) return <WarnIcon />;
	return <CheckIcon />;
}

function channelStatusColor(status: string): string {
	if (status === "connected") return "var(--ok)";
	return status === "error" ? "var(--error)" : "var(--warn)";
}

function ChannelsSummary({ channels }: { channels: SummaryChannel[] }): VNode {
	return (
		<SummaryRow icon={channelSummaryIcon(channels)} label="Channels">
			{channels.length ? (
				<div className="flex flex-col gap-1">
					{channels.map((channel) => (
						<div key={channel.account_id} className="flex items-center gap-1">
							<span style={`color:${channelStatusColor(channel.status)}`}>{"\u25CF"}</span>
							<span className="font-medium text-[var(--text)]">{channel.type}</span>:{" "}
							{channel.name || channel.account_id}
							<span>({channel.status})</span>
						</div>
					))}
				</div>
			) : (
				<>No channels configured</>
			)}
		</SummaryRow>
	);
}

function SkillsSummary({ skills }: { skills: SummarySkills | null }): VNode | null {
	if (!skills) return null;
	return (
		<SummaryRow icon={skills.enabledCategories > 0 ? <CheckIcon /> : <InfoIcon />} label="Skills">
			<span className="font-medium text-[var(--text)]">{skills.enabledSkills}</span> skills enabled across{" "}
			<span className="font-medium text-[var(--text)]">
				{skills.enabledCategories}/{skills.totalCategories}
			</span>{" "}
			categories
		</SummaryRow>
	);
}

function MemorySummary({ memory }: { memory: SummaryData["mem"] }): VNode {
	const lowMemory = !!(memory?.total && memory.total < LOW_MEMORY_THRESHOLD);
	return (
		<SummaryRow icon={lowMemory ? <WarnIcon /> : <CheckIcon />} label="System Memory">
			{memory ? (
				<>
					Total: <span className="font-medium text-[var(--text)]">{formatMemBytes(memory.total)}</span> Available:{" "}
					<span className="font-medium text-[var(--text)]">{formatMemBytes(memory.available)}</span>
					{lowMemory && (
						<div className="text-[var(--warn)] mt-1">
							Low memory detected. Consider upgrading to an instance with more RAM.
						</div>
					)}
				</>
			) : (
				<>Memory info unavailable</>
			)}
		</SummaryRow>
	);
}

function SandboxSummary({ sandbox }: { sandbox: SandboxGonInfo | null }): VNode {
	const enabled = sandbox?.mode === "On";
	return (
		<SummaryRow icon={enabled ? <CheckIcon /> : <InfoIcon />} label="Sandbox">
			{enabled ? (
				<>
					Backend: <span className="font-medium text-[var(--text)]">{sandbox.backend}</span>
				</>
			) : (
				<>Mode: Off — commands execute directly on the host</>
			)}
		</SummaryRow>
	);
}

function VersionSummary({ update }: { update: SummaryData["update"] }): VNode {
	return (
		<SummaryRow icon={update?.available ? <WarnIcon /> : <CheckIcon />} label="Version">
			{update?.available ? (
				<>
					Update available:{" "}
					<a
						href={update.release_url || "#"}
						target="_blank"
						rel="noopener"
						className="text-[var(--accent)] underline font-medium"
					>
						{update.latest_version}
					</a>
				</>
			) : (
				<>You are running the latest version.</>
			)}
		</SummaryRow>
	);
}

function VoiceSummary({ enabled, voice }: { enabled: boolean; voice: SummaryVoice | null }): VNode | null {
	if (!enabled) return null;
	const enabledStt = voice?.stt.filter((provider) => provider.enabled).map((provider) => provider.name) || [];
	const enabledTts = voice?.tts.filter((provider) => provider.enabled).map((provider) => provider.name) || [];
	const anyEnabled = enabledStt.length > 0 || enabledTts.length > 0;
	return (
		<SummaryRow icon={anyEnabled ? <CheckIcon /> : <InfoIcon />} label="Voice">
			{voice ? (
				anyEnabled ? (
					<div className="flex flex-col gap-0.5">
						{enabledStt.length > 0 && (
							<div>
								STT: <span className="font-medium text-[var(--text)]">{enabledStt.join(", ")}</span>
							</div>
						)}
						{enabledTts.length > 0 && (
							<div>
								TTS: <span className="font-medium text-[var(--text)]">{enabledTts.join(", ")}</span>
							</div>
						)}
					</div>
				) : (
					<>No voice providers enabled</>
				)
			) : (
				<>Voice providers unavailable</>
			)}
		</SummaryRow>
	);
}

function SummaryActions({
	data,
	onBack,
	onFinish,
}: {
	data: SummaryData;
	onBack: () => void;
	onFinish: () => void;
}): VNode {
	return (
		<div className="flex flex-wrap items-center gap-3 mt-1">
			<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
				{t("common:actions.back")}
			</button>
			<div className="flex-1" />
			<button type="button" className="provider-btn" onClick={onFinish}>
				{data.identity?.emoji || ""} {data.identity?.name || "Your agent"}, reporting for duty
			</button>
		</div>
	);
}

// ── SummaryStep ─────────────────────────────────────────────

function SummaryStep({ onBack, onFinish }: { onBack: () => void; onFinish: () => void }): VNode {
	const [loading, setLoading] = useState(true);
	const [data, setData] = useState<SummaryData | null>(null);

	useEffect(() => {
		let cancelled = false;
		fetchSummaryData().then((summary) => {
			if (cancelled) return;
			setData(summary);
			setLoading(false);
		});
		return () => {
			cancelled = true;
		};
	}, []);

	if (loading || !data) {
		return (
			<div className="flex flex-col items-center justify-center gap-3 min-h-[200px]">
				<div className="inline-block w-8 h-8 border-2 border-[var(--border)] border-t-[var(--accent)] rounded-full animate-spin" />
				<div className="text-sm text-[var(--muted)]">{t("onboarding:summary.loadingSummary")}</div>
			</div>
		);
	}

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:summary.title")}</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Overview of your configuration. You can change any of these later in Settings.
			</p>
			<div className="flex flex-col gap-2">
				<IdentitySummary identity={data.identity} />
				<ProvidersSummary providers={data.providers} activeModel={localStorage.getItem("chelix-model")} />
				<ChannelsSummary channels={data.channels} />
				<SkillsSummary skills={data.skills} />
				<MemorySummary memory={data.mem} />
				<SandboxSummary sandbox={data.sandbox} />
				<VersionSummary update={data.update} />
				<VoiceSummary enabled={data.voiceEnabled} voice={data.voice} />
			</div>
			<SummaryActions data={data} onBack={onBack} onFinish={onFinish} />
		</div>
	);
}

// ── Main page component ─────────────────────────────────────

interface OnboardingAuthStatus {
	setup_required?: boolean;
	auth_disabled?: boolean;
	localhost_only?: boolean;
}

interface OnboardingAuthPlan {
	authNeeded: boolean;
	authSkippable: boolean;
	initialStep: number;
}

interface OnboardingPlan {
	steps: string[];
	stepIndex: number;
	importStep: number;
	llmStep: number;
	voiceStep: number;
	skillsStep: number;
	channelStep: number;
	identityStep: number;
	summaryStep: number;
}

function hideOnboardingChrome(): () => void {
	const restorable = [
		document.querySelector("header") as HTMLElement | null,
		document.getElementById("navPanel"),
		document.getElementById("sessionsPanel"),
		document.getElementById("burgerBtn"),
		document.getElementById("sessionsToggle"),
	];
	for (const element of restorable) {
		if (element) element.style.display = "none";
	}
	const authBanner = document.getElementById("authDisabledBanner");
	if (authBanner) authBanner.style.display = "none";
	return () => {
		for (const element of restorable) {
			if (element) element.style.display = "";
		}
	};
}

function onboardingAuthPlan(status: OnboardingAuthStatus | null): OnboardingAuthPlan {
	const authNeeded = !!(status?.setup_required || (status?.auth_disabled && !status.localhost_only));
	return {
		authNeeded,
		authSkippable: authNeeded && !status?.setup_required,
		initialStep: authNeeded ? 0 : 1,
	};
}

async function fetchOnboardingAuthPlan(): Promise<OnboardingAuthPlan> {
	try {
		const response = await fetch("/api/auth/status");
		return onboardingAuthPlan(response.ok ? ((await response.json()) as OnboardingAuthStatus) : null);
	} catch {
		return onboardingAuthPlan(null);
	}
}

function buildOnboardingPlan(
	step: number,
	authNeeded: boolean,
	voiceAvailable: boolean,
	importDetected: boolean,
): OnboardingPlan {
	const allLabels = [t("onboarding:steps.security")];
	if (importDetected) allLabels.push(t("onboarding:steps.import"));
	allLabels.push(t("onboarding:steps.llm"));
	if (voiceAvailable) allLabels.push(t("onboarding:steps.voice"));
	allLabels.push(
		t("onboarding:steps.skills"),
		t("onboarding:steps.channel"),
		t("onboarding:steps.identity"),
		t("onboarding:steps.summary"),
	);
	let nextIndex = 1;
	const importStep = importDetected ? nextIndex++ : -1;
	const llmStep = nextIndex++;
	const voiceStep = voiceAvailable ? nextIndex++ : -1;
	const skillsStep = nextIndex++;
	const channelStep = nextIndex++;
	const identityStep = nextIndex++;
	return {
		steps: authNeeded ? allLabels : allLabels.slice(1),
		stepIndex: authNeeded ? step : step - 1,
		importStep,
		llmStep,
		voiceStep,
		skillsStep,
		channelStep,
		identityStep,
		summaryStep: nextIndex,
	};
}

interface OnboardingStepContentProps {
	step: number;
	plan: OnboardingPlan;
	authNeeded: boolean;
	authSkippable: boolean;
	importDetected: boolean;
	onNext: () => void;
	onBack: () => void;
	onFinish: () => void;
}

function OnboardingStepContent(props: OnboardingStepContentProps): VNode | null {
	if (props.step === 0) return <AuthStep onNext={props.onNext} skippable={props.authSkippable} />;
	if (props.step === props.plan.importStep) {
		return <ImportStep onNext={props.onNext} onBack={props.authNeeded ? props.onBack : null} />;
	}
	if (props.step === props.plan.llmStep) {
		return (
			<ProviderStep onNext={props.onNext} onBack={props.authNeeded || props.importDetected ? props.onBack : null} />
		);
	}
	if (props.step === props.plan.voiceStep) return <VoiceStep onNext={props.onNext} onBack={props.onBack} />;
	if (props.step === props.plan.skillsStep) return <SkillsStep onNext={props.onNext} onBack={props.onBack} />;
	if (props.step === props.plan.channelStep) return <ChannelStep onNext={props.onNext} onBack={props.onBack} />;
	if (props.step === props.plan.identityStep) return <IdentityStep onNext={props.onNext} onBack={props.onBack} />;
	if (props.step === props.plan.summaryStep) return <SummaryStep onBack={props.onBack} onFinish={props.onFinish} />;
	return null;
}

function OnboardingServerInfo({ startedAt, version }: { startedAt: number | null; version: string }): VNode | null {
	if (!(startedAt || version)) return null;
	return (
		<div className="text-xs text-[var(--muted)] text-center mt-4 pt-3 border-t border-[var(--border)]">
			{startedAt && (
				<span>
					Server started <time data-epoch-ms={startedAt} />
				</span>
			)}
			{startedAt && version && <span> {"\u00b7"} </span>}
			{version && (
				<span>
					{t("onboarding:summary.versionLabel")} v{version}
				</span>
			)}
		</div>
	);
}

function OnboardingPage(): VNode {
	const [step, setStep] = useState(-1); // -1 = checking
	const [authNeeded, setAuthNeeded] = useState(false);
	const [authSkippable, setAuthSkippable] = useState(false);
	const [voiceAvailable] = useState(() => getGon("voice_enabled") === true);

	useEffect(hideOnboardingChrome, []);

	useEffect(() => {
		fetchOnboardingAuthPlan().then((authPlan) => {
			setAuthNeeded(authPlan.authNeeded);
			setAuthSkippable(authPlan.authSkippable);
			if (!authPlan.authNeeded) ensureWsConnected();
			setStep(authPlan.initialStep);
		});
	}, []);

	if (step === -1) {
		return (
			<div className="onboarding-card">
				<div className="text-sm text-[var(--muted)]">{t("common:status.loading")}</div>
			</div>
		);
	}

	const importDetected = getGon("claude_detected") === true;
	const plan = buildOnboardingPlan(step, authNeeded, voiceAvailable, importDetected);

	function goNext(): void {
		if (step === plan.summaryStep) window.location.assign(preferredChatPath());
		else setStep(step + 1);
	}

	function goFinish(): void {
		window.location.assign(preferredChatPath());
	}

	function goBack(): void {
		setStep(Math.max(authNeeded ? 0 : 1, step - 1));
	}

	return (
		<div className="onboarding-card">
			<StepIndicator steps={plan.steps} current={plan.stepIndex} />
			<div className="mt-6">
				<OnboardingStepContent
					step={step}
					plan={plan}
					authNeeded={authNeeded}
					authSkippable={authSkippable}
					importDetected={importDetected}
					onNext={goNext}
					onBack={goBack}
					onFinish={goFinish}
				/>
			</div>
			<OnboardingServerInfo
				startedAt={getGon("started_at") as number | null}
				version={String(getGon("version") || "").trim()}
			/>
		</div>
	);
}

// ── Page registration ───────────────────────────────────────

let containerRef: HTMLElement | null = null;

export function mountOnboarding(container: HTMLElement): void {
	containerRef = container;
	container.style.cssText =
		"display:flex;align-items:flex-start;justify-content:center;min-height:100vh;padding:max(0.75rem, env(safe-area-inset-top)) max(0.75rem, env(safe-area-inset-right)) max(0.75rem, env(safe-area-inset-bottom)) max(0.75rem, env(safe-area-inset-left));box-sizing:border-box;width:100%;max-width:100vw;overflow-x:hidden;overflow-y:auto;";
	render(<OnboardingPage />, container);
}

export function unmountOnboarding(): void {
	if (containerRef) render(null, containerRef);
	containerRef = null;
}
