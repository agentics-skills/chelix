// ── Session history pagination UI controller ───────────────────

import { chatAddMsg } from "../chat-ui";
import * as S from "../state";
import { getSessionHistory, replaceSessionHistory } from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import type { HistoryMessage } from "../types/session";
import type { ReasoningContent } from "../types/ws-events";

import {
	fetchSessionHistoryViaHttp,
	getHistoryPaginationState,
	type HistoryPaginationState,
	type HistoryPayload,
	mergeHistoryPages,
	SESSION_HISTORY_PAGE_LIMIT,
	setHistoryPaginationLoading,
	setHistoryPaginationState,
} from "./session-history";
import { renderHistory, type SearchContext } from "./session-render";

interface ScrollSnapshot {
	height: number;
	top: number;
}

const HISTORY_AUTOLOAD_THRESHOLD_PX = 120;
let historyScrollEl: HTMLElement | null = null;
let historyScrollRaf = 0;

function isActiveSession(key: string): boolean {
	return sessionStore.activeSessionKey.value === key;
}

function canLoadOlderHistory(key: string): boolean {
	const paging = getHistoryPaginationState(key);
	return paging?.hasMore === true && typeof paging.nextCursor === "number" && paging.loadingOlder === false;
}

function maybeLoadOlderHistoryFromScroll(): void {
	if (!S.chatMsgBox) return;
	if (S.chatMsgBox.scrollTop > HISTORY_AUTOLOAD_THRESHOLD_PX) return;
	const key = sessionStore.activeSessionKey.value || S.activeSessionKey;
	if (!(key && canLoadOlderHistory(key))) return;
	void loadOlderHistoryPage(key);
}

function handleHistoryScroll(): void {
	if (historyScrollRaf) return;
	historyScrollRaf = requestAnimationFrame(() => {
		historyScrollRaf = 0;
		maybeLoadOlderHistoryFromScroll();
	});
}

function ensureHistoryScrollBinding(): void {
	const nextEl = S.chatMsgBox;
	if (historyScrollEl === nextEl) return;
	if (historyScrollEl) {
		historyScrollEl.removeEventListener("scroll", handleHistoryScroll);
	}
	historyScrollEl = nextEl;
	if (!historyScrollEl) return;
	historyScrollEl.addEventListener("scroll", handleHistoryScroll, { passive: true });
}

export function renderSessionHistory(
	key: string,
	history: HistoryMessage[],
	searchContext: SearchContext | null,
	thinkingText: ReasoningContent | null,
	totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	ensureHistoryScrollBinding();
	renderHistory(key, history, searchContext, thinkingText, totalCountHint, skipAutoScroll);
}

function resolvedHistoryTotal(totalMessages: number | null, history: HistoryMessage[]): number {
	return typeof totalMessages === "number" && Number.isInteger(totalMessages) ? totalMessages : history.length;
}

function captureScrollSnapshot(): ScrollSnapshot {
	return {
		height: S.chatMsgBox?.scrollHeight || 0,
		top: S.chatMsgBox?.scrollTop || 0,
	};
}

function restoreScrollSnapshot(snapshot: ScrollSnapshot): void {
	if (!S.chatMsgBox) return;
	S.chatMsgBox.scrollTop = Math.max(0, snapshot.top + (S.chatMsgBox.scrollHeight - snapshot.height));
}

function fetchOlderHistoryPage(key: string, paging: HistoryPaginationState): Promise<HistoryPayload> {
	if (paging.nextCursor === null) {
		throw new Error("History pagination cursor is missing");
	}
	return fetchSessionHistoryViaHttp(key, {
		cursor: paging.nextCursor,
		limit: SESSION_HISTORY_PAGE_LIMIT,
	});
}

function applyOlderHistoryPage(key: string, payload: HistoryPayload, scrollSnapshot: ScrollSnapshot): void {
	const older = Array.isArray(payload.history) ? payload.history : [];
	const current = getSessionHistory(key) || [];
	if (older.length > 0 && payload.historyCacheHit !== true) {
		replaceSessionHistory(key, mergeHistoryPages(current, older));
	}
	setHistoryPaginationState(key, payload);

	const merged = getSessionHistory(key) || [];
	const sessionMessageCount = sessionStore.getByKey(key)?.messageCount;
	const totalCountHint = Number.isInteger(sessionMessageCount)
		? (sessionMessageCount as number)
		: Number(payload.totalMessages) || merged.length;
	renderSessionHistory(key, merged, null, null, totalCountHint, true);
	restoreScrollSnapshot(scrollSnapshot);
}

function handleOlderHistoryFailure(key: string, paging: HistoryPaginationState): void {
	if (!isActiveSession(key)) return;
	const fallback = getSessionHistory(key) || [];
	setHistoryPaginationLoading(key, false);
	renderSessionHistory(key, fallback, null, null, resolvedHistoryTotal(paging.totalMessages, fallback), true);
	chatAddMsg("error", "Failed to load older messages");
}

async function loadOlderHistoryPage(key: string): Promise<void> {
	if (!(canLoadOlderHistory(key) && isActiveSession(key))) return;
	const paging = setHistoryPaginationLoading(key, true);
	if (!paging) return;

	const loadedHistory = getSessionHistory(key) || [];
	renderSessionHistory(key, loadedHistory, null, null, resolvedHistoryTotal(paging.totalMessages, loadedHistory), true);
	const scrollSnapshot = captureScrollSnapshot();

	try {
		const payload = await fetchOlderHistoryPage(key, paging);
		if (isActiveSession(key)) applyOlderHistoryPage(key, payload, scrollSnapshot);
	} catch {
		handleOlderHistoryFailure(key, paging);
	} finally {
		setHistoryPaginationLoading(key, false);
		if (isActiveSession(key)) maybeLoadOlderHistoryFromScroll();
	}
}
