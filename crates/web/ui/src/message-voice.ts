import { renderAudioPlayer } from "./helpers";

function buildSessionMediaUrl(sessionKey: string | undefined, audioPath: string | undefined): string | null {
	if (!(sessionKey && audioPath)) return null;
	const filename = String(audioPath).split("/").pop();
	if (!filename) return null;
	return `/api/sessions/${encodeURIComponent(sessionKey)}/media/${encodeURIComponent(filename)}`;
}

function formatTtsProviderLabel(provider: string): string {
	const labels: Record<string, string> = {
		elevenlabs: "ElevenLabs",
		openai: "OpenAI TTS",
		google: "Google Cloud TTS",
		piper: "Piper",
		coqui: "Coqui TTS",
	};
	return labels[provider] || provider;
}

export function upsertTtsProviderFooter(messageEl: HTMLElement | null, provider: string | undefined): void {
	if (!messageEl) return;
	const normalized = String(provider || "").trim();
	let footer = messageEl.querySelector(".msg-tts-provider-footer") as HTMLElement | null;
	if (!normalized) {
		if (footer) footer.remove();
		return;
	}
	if (!footer) {
		footer = document.createElement("div");
		footer.className = "msg-model-footer msg-tts-provider-footer";
		const actionBar = messageEl.querySelector(".msg-action-bar");
		messageEl.insertBefore(footer, actionBar || null);
	}
	footer.textContent = `TTS: ${formatTtsProviderLabel(normalized)} (${normalized})`;
}

function ensureVoicePlayerSlot(messageEl: HTMLElement | null): HTMLElement | null {
	if (!messageEl) return null;
	let slot = messageEl.querySelector(".msg-voice-player-slot") as HTMLElement | null;
	if (slot) return slot;
	slot = document.createElement("div");
	slot.className = "msg-voice-player-slot";
	messageEl.insertBefore(slot, messageEl.firstChild);
	return slot;
}

export function renderPersistedAudio(
	messageEl: HTMLElement,
	sessionKey: string | undefined,
	audioPath: string | undefined,
	autoplay: boolean,
	ttsProvider?: string,
): boolean {
	const src = buildSessionMediaUrl(sessionKey, audioPath);
	if (!src) return false;
	const slot = ensureVoicePlayerSlot(messageEl);
	if (!slot) return false;
	slot.textContent = "";
	renderAudioPlayer(slot, src, autoplay === true);
	upsertTtsProviderFooter(messageEl, ttsProvider);
	return true;
}
