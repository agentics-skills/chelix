// ── Session list: fetching, pagination, and client state ─────────

import { navigate, sessionPath } from "../router";
import * as S from "../state";
import { sessionStore } from "../stores/session-store";
import type { SessionMeta } from "../types/session";
import { showToast } from "../ui";

import { clearSessionHistoryCache } from "./session-history";

interface SessionListPage {
	sessions: SessionMeta[];
	hasMore: boolean;
	nextCursor: number | null;
	total: number | null;
}

interface SessionListPaging {
	hasMore: boolean;
	nextCursor: number | null;
	total: number | null;
	loading: boolean;
}

const SESSION_LIST_PAGE_LIMIT = 40;
const SESSION_LIST_REFRESH_LIMIT_MAX = 200;
const SESSION_LIST_SCROLL_THRESHOLD = 220;
const sessionListPaging: SessionListPaging = {
	hasMore: false,
	nextCursor: null,
	total: null,
	loading: false,
};
let sessionListPendingRefresh = false;
let sessionListScrollEl: HTMLElement | null = null;
let sessionListScrollRaf = 0;
let listEpoch = 0;

export function resetSessionList(): void {
	listEpoch += 1;
	sessionListPaging.loading = false;
	sessionListPaging.hasMore = false;
	sessionListPaging.nextCursor = null;
	sessionListPaging.total = null;
	sessionListPendingRefresh = false;
	sessionStore.setAll([]);
	S.setSessions([]);
}

export function fetchSessions(): void {
	ensureSessionListScrollBinding();
	if (sessionListPaging.loading) {
		sessionListPendingRefresh = true;
		return;
	}

	sessionListPaging.loading = true;
	const epoch = listEpoch;
	const loadedCount = Array.isArray(S.sessions) ? S.sessions.length : 0;
	const refreshLimit = Math.max(
		SESSION_LIST_PAGE_LIMIT,
		Math.min(
			Number.isInteger(loadedCount) && loadedCount > 0 ? loadedCount : SESSION_LIST_PAGE_LIMIT,
			SESSION_LIST_REFRESH_LIMIT_MAX,
		),
	);

	void fetchSessionListPage({ limit: refreshLimit })
		.then((page) => {
			if (epoch !== listEpoch) return;
			const merged = mergeSessionListPage(S.sessions as SessionMeta[], page.sessions, false);
			applySessionList(merged, page.hasMore);
			applySessionListPaging(page);
		})
		.catch((error: unknown) => {
			if (epoch === listEpoch)
				showToast(error instanceof Error ? error.message : "Session list refresh failed", "error");
		})
		.finally(() => {
			if (epoch !== listEpoch) return;
			sessionListPaging.loading = false;
			if (sessionListPendingRefresh) {
				sessionListPendingRefresh = false;
				fetchSessions();
				return;
			}
			maybeLoadMoreSessionsFromScroll();
		});
}

function toValidCursor(value: unknown): number | null {
	const parsed = Number(value);
	if (!Number.isInteger(parsed) || parsed < 0) return null;
	return parsed;
}

function parseSessionListPayload(payload: unknown): SessionListPage {
	if (Array.isArray(payload)) {
		return {
			sessions: payload as SessionMeta[],
			hasMore: false,
			nextCursor: null,
			total: payload.length,
		};
	}

	const obj = payload as Record<string, unknown> | null;
	const list = Array.isArray(obj?.sessions) ? (obj?.sessions as SessionMeta[]) : [];
	const nextCursor = toValidCursor(obj?.nextCursor);
	const hasMore = obj?.hasMore === true && nextCursor !== null;
	const total = Number(obj?.total);
	return {
		sessions: list,
		hasMore,
		nextCursor: hasMore ? nextCursor : null,
		total: Number.isInteger(total) && total >= 0 ? total : null,
	};
}

function mergeSessionListPage(
	existingSessions: SessionMeta[],
	incomingSessions: SessionMeta[],
	append: boolean,
): SessionMeta[] {
	const existing = Array.isArray(existingSessions) ? existingSessions : [];
	const incoming = Array.isArray(incomingSessions) ? incomingSessions : [];

	const oldByKey: Record<string, SessionMeta> = {};
	for (const old of existing) {
		if (!old?.key) continue;
		oldByKey[old.key] = old;
	}

	function withLocalFlags(session: SessionMeta): SessionMeta {
		if (!session?.key) return session;
		const prev = oldByKey[session.key];
		if (!prev) return session;
		const merged = { ...session };
		if (prev._localUnread) merged._localUnread = true;
		if (prev._replying) merged._replying = true;
		return merged;
	}

	if (!append) {
		return incoming.map((session) => withLocalFlags(session));
	}

	const result = existing.slice();
	const indexByKey: Record<string, number> = {};
	for (let i = 0; i < result.length; i += 1) {
		const key = result[i]?.key;
		if (!key) continue;
		indexByKey[key] = i;
	}

	for (const session of incoming) {
		if (!session?.key) continue;
		const next = withLocalFlags(session);
		const idx = indexByKey[session.key];
		if (Number.isInteger(idx)) {
			result[idx] = { ...result[idx], ...next };
			continue;
		}
		indexByKey[session.key] = result.length;
		result.push(next);
	}

	return result;
}

function applySessionList(sessions: SessionMeta[], retainOmittedActive = true): void {
	sessionStore.setListed(sessions, retainOmittedActive);
	S.setSessions(sessionStore.sessions.value.map((session) => session.toMeta()));
	renderSessionList();
}

function applySessionListPaging(page: SessionListPage): void {
	sessionListPaging.hasMore = page.hasMore === true && Number.isInteger(page.nextCursor);
	sessionListPaging.nextCursor = sessionListPaging.hasMore ? page.nextCursor : null;
	sessionListPaging.total = Number.isInteger(page.total) ? page.total : null;
}

async function fetchSessionListPage(options?: { cursor?: number; limit?: number }): Promise<SessionListPage> {
	const opts = options || {};
	const query = new URLSearchParams();
	if (Number.isInteger(opts.cursor) && (opts.cursor as number) >= 0) {
		query.set("cursor", String(opts.cursor));
	}
	if (Number.isInteger(opts.limit) && (opts.limit as number) > 0) {
		query.set("limit", String(opts.limit));
	}

	let url = "/api/sessions";
	const qs = query.toString();
	if (qs) url += `?${qs}`;

	const response = await fetch(url, {
		headers: { Accept: "application/json" },
	});
	let payload: unknown = null;
	try {
		payload = await response.json();
	} catch {
		payload = null;
	}
	if (!response.ok) {
		throw new Error(`Failed to fetch sessions (${response.status})`);
	}
	return parseSessionListPayload(payload);
}

function shouldLoadMoreSessions(): boolean {
	const el = S.$("sessionList");
	if (!el) return false;
	if (el.clientHeight <= 0) return false;
	if (sessionListPaging.loading) return false;
	if (!(sessionListPaging.hasMore && Number.isInteger(sessionListPaging.nextCursor))) return false;
	const distance = el.scrollHeight - (el.scrollTop + el.clientHeight);
	return distance <= SESSION_LIST_SCROLL_THRESHOLD;
}

async function loadMoreSessionsPage(): Promise<void> {
	if (!shouldLoadMoreSessions()) return;
	sessionListPaging.loading = true;
	const epoch = listEpoch;
	try {
		const page = await fetchSessionListPage({
			cursor: sessionListPaging.nextCursor as number,
			limit: SESSION_LIST_PAGE_LIMIT,
		});
		if (epoch !== listEpoch) return;
		const merged = mergeSessionListPage(S.sessions as SessionMeta[], page.sessions, true);
		applySessionList(merged);
		if (page.sessions.length === 0) {
			applySessionListPaging({
				hasMore: false,
				nextCursor: null,
				total: page.total,
				sessions: [],
			});
		} else {
			applySessionListPaging(page);
		}
	} catch (error: unknown) {
		if (epoch === listEpoch) showToast(error instanceof Error ? error.message : "Session list paging failed", "error");
	} finally {
		if (epoch === listEpoch) {
			sessionListPaging.loading = false;
			if (sessionListPendingRefresh) {
				sessionListPendingRefresh = false;
				fetchSessions();
			} else {
				maybeLoadMoreSessionsFromScroll();
			}
		}
	}
}

function maybeLoadMoreSessionsFromScroll(): void {
	if (!shouldLoadMoreSessions()) return;
	void loadMoreSessionsPage();
}

function handleSessionListScroll(): void {
	if (sessionListScrollRaf) return;
	sessionListScrollRaf = requestAnimationFrame(() => {
		sessionListScrollRaf = 0;
		maybeLoadMoreSessionsFromScroll();
	});
}

function ensureSessionListScrollBinding(): void {
	const nextEl = S.$("sessionList");
	if (sessionListScrollEl === nextEl) return;
	if (sessionListScrollEl) {
		sessionListScrollEl.removeEventListener("scroll", handleSessionListScroll);
	}
	sessionListScrollEl = nextEl;
	if (!sessionListScrollEl) return;
	sessionListScrollEl.addEventListener("scroll", handleSessionListScroll, { passive: true });
}

export function renderSessionList(): void {
	ensureSessionListScrollBinding();
	maybeLoadMoreSessionsFromScroll();
}

export function setSessionReplying(key: string, replying: boolean): void {
	const session = sessionStore.getByKey(key);
	if (session) session.replying.value = replying;
	const entry = (S.sessions as SessionMeta[]).find((sessionEntry) => sessionEntry.key === key);
	if (entry) entry._replying = replying;
}

export function setSessionActiveRunId(key: string, runId: string | null): void {
	const session = sessionStore.getByKey(key);
	if (session) session.activeRunId.value = runId || null;
	const entry = (S.sessions as SessionMeta[]).find((sessionEntry) => sessionEntry.key === key);
	if (entry) (entry as SessionMeta & { _activeRunId?: string | null })._activeRunId = runId || null;
}

export function setSessionUnread(key: string, unread: boolean): void {
	const session = sessionStore.getByKey(key);
	if (session) session.localUnread.value = unread;
	const entry = (S.sessions as SessionMeta[]).find((sessionEntry) => sessionEntry.key === key);
	if (entry) entry._localUnread = unread;
}

export function removeSessionFromClientState(
	key: string,
	options?: { nextKey?: string; navigateIfActive?: boolean },
): boolean {
	const opts = options || {};
	if (!key) return false;
	const removedActive = sessionStore.activeSessionKey.value === key;
	const removed = sessionStore.remove(key);
	if (!removed) return false;
	const nextKey = opts.nextKey || sessionStore.activeSessionKey.value || "main";
	if (removedActive && nextKey !== sessionStore.activeSessionKey.value) sessionStore.setActive(nextKey);
	clearSessionHistoryCache(key);
	S.setSessions((S.sessions as SessionMeta[]).filter((session) => session.key !== key));
	renderSessionList();
	if (!removedActive) return true;
	S.setActiveSessionKey(nextKey);
	if (opts.navigateIfActive && location.pathname.startsWith("/chats/")) navigate(sessionPath(nextKey));
	return true;
}
