import * as S from "../state";
import { clearSessionHistory, getHistoryWindow } from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import type { SessionMeta } from "../types/session";
import type { UiHistoryPage } from "../types/ui-history";

export const SESSION_HISTORY_PAGE_LIMIT = 120;

export function clearSessionHistoryCache(key?: string): void {
	clearSessionHistory(key);
}

export async function fetchSessionHistoryViaHttp(
	key: string,
	options: { before?: number; after?: number; around?: string; generation?: string; limit?: number } = {},
): Promise<UiHistoryPage> {
	const query = new URLSearchParams();
	for (const [name, value] of Object.entries(options)) if (value !== undefined) query.set(name, String(value));
	const response = await fetch(`/api/sessions/${encodeURIComponent(key)}/history?${query}`, {
		headers: { Accept: "application/json" },
	});
	if (!response.ok) throw new Error(`Failed to load session history (${response.status})`);
	return response.json();
}

export function syncHistoryState(key: string): void {
	const window = getHistoryWindow(key);
	if (!window) return;
	const viewing = key === sessionStore.activeSessionKey.value && location.pathname.startsWith("/chats");
	const entry = sessionStore.getByKey(key);
	if (entry) {
		entry.syncCounts(window.totalMessages, viewing ? window.totalMessages : entry.lastSeenMessageCount);
		entry.localUnread.value = !viewing && entry.lastSeenMessageCount < window.totalMessages;
	}
	const listed = (S.sessions as SessionMeta[]).find((session) => session.key === key);
	if (listed) {
		listed.messageCount = window.totalMessages;
		if (viewing) listed.lastSeenMessageCount = window.totalMessages;
		listed._localUnread = !viewing && Number(listed.lastSeenMessageCount) < window.totalMessages;
	}
}
