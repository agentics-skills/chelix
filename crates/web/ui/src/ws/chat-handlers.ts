import { chatAddMsg, setComposerStopButton, updateTokenBar } from "../chat-ui";
import { maybeRefreshFullContext } from "../pages/ChatPage";
import { replaceQueuedPromptsDock } from "../pages/chat/prompt-queue";
import { currentPrefix } from "../router";
import { fetchSessions, setSessionActiveRunId, setSessionReplying } from "../sessions";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import type { ChatPayload } from "../types/ws-events";

const compacting = new Map<string, HTMLElement>();

function running(payload: ChatPayload, key: string, active: boolean): void {
	if (payload.runId) setSessionActiveRunId(key, payload.runId);
	setSessionReplying(key, true);
	if (active) setComposerStopButton(true, key);
}

function terminal(key: string, active: boolean): void {
	setSessionReplying(key, false);
	setSessionActiveRunId(key, null);
	sessionStore.getByKey(key)?.resetStreamState();
	if (!active) return;
	setComposerStopButton(false);
	S.setVoicePending(false);
	maybeRefreshFullContext();
}

function compact(payload: ChatPayload, key: string, active: boolean): void {
	if (payload.phase === "start") {
		if (!active || compacting.has(key)) return;
		const element = chatAddMsg("system", "Summarizing conversation…");
		if (element) compacting.set(key, element);
		return;
	}
	compacting.get(key)?.remove();
	compacting.delete(key);
	if (!active || payload.phase !== "done") return;
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	updateTokenBar();
}

function voicePending(key: string, active: boolean): void {
	const session = sessionStore.getByKey(key);
	if (session) session.voicePending.value = true;
	if (active) S.setVoicePending(true);
}

export function handleChatEvent(payload: ChatPayload): void {
	const key = payload.status?.sessionKey || payload.sessionKey;
	if (!key) return;
	if (!sessionStore.getByKey(key)) fetchSessions();
	const active = key === sessionStore.activeSessionKey.value && currentPrefix === "/chats";
	switch (payload.state) {
		case "thinking":
		case "segment_start":
		case "retrying":
			running(payload, key, active);
			return;
		case "voice_pending":
			voicePending(key, active);
			return;
		case "final":
		case "error":
		case "aborted":
		case "session_cleared":
			terminal(key, active);
			return;
		case "prompt_queue":
			if (active && payload.status) replaceQueuedPromptsDock(payload.status);
			return;
		case "auto_compact":
		case "compact":
			compact(payload, key, active);
			return;
	}
}
