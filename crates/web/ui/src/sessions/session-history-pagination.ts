// ── Session history pagination UI controller ───────────────────

import { chatAddMsg } from "../chat-ui";
import * as S from "../state";
import { getSessionHistory, replaceSessionHistory, retainSessionHistory } from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import type { HistoryMessage } from "../types/session";

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
import { prependHistoryPage, renderHistory, type SearchContext } from "./session-render";

// How much history is kept ready above the viewport. Loading is driven by this
// distance rather than by scroll events, so it cannot stall when a page adds
// little height.
const HISTORY_HEADROOM_PX = 1200;
let historyScrollEl: HTMLElement | null = null;
let historyScrollRaf = 0;
// Session whose fill loop is running, so concurrent scroll events join it
// instead of starting a second one. Keyed by session rather than global: a
// switch must be able to start filling the newly opened chat even while the
// previous one is still finishing a request.
let historyFillKey: string | null = null;

function isActiveSession(key: string): boolean {
	return sessionStore.activeSessionKey.value === key;
}

function canLoadOlderHistory(key: string): boolean {
	const paging = getHistoryPaginationState(key);
	return paging?.hasMore === true && typeof paging.nextCursor === "number" && paging.loadingOlder === false;
}

/// Load older pages until the headroom above the viewport is filled.
///
/// The stop condition is the distance to the top, not the arrival of a page: a
/// page holds a fixed number of messages, and how much height they add is
/// unknown until they are rendered. Looping on the distance keeps short pages
/// from leaving the user at a standstill.
async function fillHistoryHeadroom(): Promise<void> {
	const key = sessionStore.activeSessionKey.value || S.activeSessionKey;
	if (!key || historyFillKey === key) return;
	historyFillKey = key;
	try {
		while (S.chatMsgBox && S.chatMsgBox.scrollTop < HISTORY_HEADROOM_PX) {
			// The user may have switched away mid-request; that session owns its
			// own fill loop and this one must stop.
			if (!isActiveSession(key)) return;
			if (!canLoadOlderHistory(key)) return;
			const loaded = await loadOlderHistoryPage(key);
			if (!loaded) return;
		}
	} finally {
		if (historyFillKey === key) historyFillKey = null;
	}
}

function handleHistoryScroll(): void {
	if (historyScrollRaf) return;
	historyScrollRaf = requestAnimationFrame(() => {
		historyScrollRaf = 0;
		void fillHistoryHeadroom();
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
	totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	ensureHistoryScrollBinding();
	// This session is the one on screen: its pages must survive, and the
	// previously retained session becomes evictable again.
	retainSessionHistory(key);
	renderHistory(key, history, searchContext, totalCountHint, skipAutoScroll);
	// A short first page can leave the viewport already at the top, where no
	// scroll event will ever arrive to start loading.
	void fillHistoryHeadroom();
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

/// Add one older page to the cache and to the DOM.
///
/// The page is rendered on its own and inserted above the existing messages.
/// Nothing already rendered is rebuilt, so live tool bubbles keep their sockets
/// and the reading position is preserved.
async function applyOlderHistoryPage(key: string, payload: HistoryPayload): Promise<void> {
	const older = Array.isArray(payload.history) ? payload.history : [];
	setHistoryPaginationState(key, payload);
	if (older.length === 0 || payload.historyCacheHit === true) return;
	const tail = getSessionHistory(key) || [];
	replaceSessionHistory(key, mergeHistoryPages(tail, older));
	// The server pages strictly below the cursor, so an older page never repeats
	// a message that is already on screen.
	await prependHistoryPage(key, older);
}

/// Load and insert one older page. Returns whether the caller may continue.
async function loadOlderHistoryPage(key: string): Promise<boolean> {
	if (!(canLoadOlderHistory(key) && isActiveSession(key))) return false;
	const paging = setHistoryPaginationLoading(key, true);
	if (!paging) return false;
	try {
		const payload = await fetchOlderHistoryPage(key, paging);
		if (!isActiveSession(key)) return false;
		await applyOlderHistoryPage(key, payload);
		return true;
	} catch (error) {
		if (isActiveSession(key)) {
			chatAddMsg("error", `Failed to load older messages: ${error instanceof Error ? error.message : String(error)}`);
		}
		return false;
	} finally {
		setHistoryPaginationLoading(key, false);
	}
}
