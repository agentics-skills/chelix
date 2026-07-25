import * as S from "../state";
import { clearSessionHistory } from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import type { SessionMeta } from "../types";

import { clearHistoryPaginationState } from "./session-history";

function previewFromEntry(entry: SessionMeta | undefined, fallback: string | null | undefined): string {
	if (entry && "preview" in entry) return entry.preview || "";
	return fallback || "";
}

export function markSessionTailLocallyTruncated(key: string, keptCount: number, entry?: SessionMeta): void {
	if (!key) return;
	const nextCount = Math.max(0, keptCount);
	const now = Date.now();

	const session = sessionStore.getByKey(key);
	if (session) {
		session.syncCounts(
			nextCount,
			key === S.activeSessionKey ? nextCount : Math.min(session.lastSeenMessageCount, nextCount),
		);
		session.preview = previewFromEntry(entry, session.preview);
		session.updatedAt = entry?.updatedAt || now;
		session.replying.value = false;
		session.activeRunId.value = null;
		session.lastHistoryIndex.value = nextCount - 1;
		if (entry?.version) session.version = entry.version;
		session.dataVersion.value++;
	}

	const legacy = (S.sessions as SessionMeta[]).find((s) => s.key === key);
	if (legacy) {
		legacy.messageCount = nextCount;
		legacy.lastSeenMessageCount =
			key === S.activeSessionKey ? nextCount : Math.min(legacy.lastSeenMessageCount || 0, nextCount);
		legacy.preview = previewFromEntry(entry, legacy.preview);
		legacy.updatedAt = entry?.updatedAt || now;
		legacy._localUnread = false;
		legacy._replying = false;
		if (entry?.version) legacy.version = entry.version;
	}

	clearSessionHistory(key);
	clearHistoryPaginationState(key);
}
