// ── Sessions facade and top-level actions ───────────────────────

import { sendRpc } from "./helpers";
import { currentPrefix, navigate, sessionPath } from "./router";
import { setSessionAgent as setSessionAgentImpl } from "./sessions/session-agent";
import * as sessionHistory from "./sessions/session-history";
import * as sessionList from "./sessions/session-list";
import {
	refreshWelcomeCardIfNeeded as refreshWelcomeCardIfNeededImpl,
	type SearchContext as SessionSearchContext,
	updateChatSessionHeader as updateChatSessionHeaderImpl,
} from "./sessions/session-render";
import {
	clearActiveSession as clearActiveSessionImpl,
	switchSession as switchSessionImpl,
} from "./sessions/session-switch";
import { markSessionTailLocallyTruncated as markSessionTailLocallyTruncatedImpl } from "./sessions/session-tail";
import * as S from "./state";
import { projectStore } from "./stores/project-store";
import { clearSessionHistory } from "./stores/session-history-cache";
import { sessionStore } from "./stores/session-store";
import type { SessionMeta } from "./types/session";
import { confirmDialog } from "./ui";

// This module is the intentional runtime and E2E compatibility entry point for session APIs.
export type SearchContext = SessionSearchContext;
export const setSessionAgent = setSessionAgentImpl;
export const cacheOutgoingUserMessage = sessionHistory.cacheOutgoingUserMessage;
export const cacheSessionHistoryMessage = sessionHistory.cacheSessionHistoryMessage;
export const clearHistoryPaginationState = sessionHistory.clearHistoryPaginationState;
export const clearSessionHistoryCache = sessionHistory.clearSessionHistoryCache;
export const appendingAddsBubble = sessionHistory.appendingAddsBubble;
export const bumpSessionCount = sessionList.bumpSessionCount;
export const fetchSessions = sessionList.fetchSessions;
export const markSessionLocallyCleared = sessionList.markSessionLocallyCleared;
export const removeSessionFromClientState = sessionList.removeSessionFromClientState;
export const renderSessionList = sessionList.renderSessionList;
export const seedSessionPreviewFromUserText = sessionList.seedSessionPreviewFromUserText;
export const setSessionActiveRunId = sessionList.setSessionActiveRunId;
export const setSessionReplying = sessionList.setSessionReplying;
export const setSessionUnread = sessionList.setSessionUnread;
export const refreshWelcomeCardIfNeeded = refreshWelcomeCardIfNeededImpl;
export const updateChatSessionHeader = updateChatSessionHeaderImpl;
export const clearActiveSession = clearActiveSessionImpl;
export const switchSession = switchSessionImpl;
export const markSessionTailLocallyTruncated = markSessionTailLocallyTruncatedImpl;

const newSessionBtn = S.$("newSessionBtn") as HTMLElement;
newSessionBtn.addEventListener("click", () => {
	const id = crypto.randomUUID
		? crypto.randomUUID()
		: ([1e7].toString() + -1e3 + -4e3 + -8e3 + -1e11).replace(/[018]/g, (char) =>
				(Number(char) ^ (crypto.getRandomValues(new Uint8Array(1))[0] & (15 >> (Number(char) / 4)))).toString(16),
			);
	const key = `session:${id}`;
	const filterId = projectStore.projectFilterId.value;
	if (currentPrefix === "/chats") {
		switchSession(key, null, filterId || undefined);
	} else {
		navigate(sessionPath(key));
	}
});

export function isArchivableSession(session: SessionMeta): boolean {
	return (
		session.key !== "main" &&
		((session as SessionMeta & { activeChannel?: boolean }).activeChannel !== true || session.archived === true)
	);
}

function isClearableSession(session: SessionMeta): boolean {
	const isChannelSessionKey =
		session.key.startsWith("telegram:") ||
		session.key.startsWith("discord:") ||
		session.key.startsWith("slack:") ||
		session.key.startsWith("matrix:");
	return session.key !== "main" && !session.key.startsWith("cron:") && !isChannelSessionKey && !session.channelBinding;
}

export function clearAllSessions(): Promise<{ ok: boolean; skipped?: boolean; cancelled?: boolean }> {
	const allSessions = sessionStore.sessions.value;
	const count = allSessions.filter((session) => isClearableSession(session as unknown as SessionMeta)).length;
	if (count === 0) {
		return Promise.resolve({ ok: true, skipped: true });
	}
	return confirmDialog(
		`Delete ${count} session${count !== 1 ? "s" : ""}? Main, channel-bound, and cron sessions will be kept.`,
	).then((yes) => {
		if (!yes) return { ok: false, cancelled: true };
		return sendRpc("sessions.clear_all", {}).then((res) => {
			if (!res?.ok) return res;
			clearSessionHistory();
			const active = sessionStore.getByKey(sessionStore.activeSessionKey.value);
			if (active && isClearableSession(active as unknown as SessionMeta)) {
				switchSession("main");
			}
			fetchSessions();
			return res;
		});
	});
}

document.addEventListener("chelix:render-session-list", renderSessionList);
