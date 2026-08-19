// ── Session history: loading, caching, pagination ─────────────────

import * as S from "../state";
import {
	clearSessionHistory,
	getHistoryRevision,
	getSessionHistory,
	upsertSessionHistoryMessage,
} from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import type { HistoryMessage, SessionMeta } from "../types/session";
import { hasVisibleReasoning, isReasoningContent } from "../types/ws-events";

interface ChatParams {
	content?: unknown[];
	text?: string;
	_seq?: number | null;
}

export interface HistoryPaginationState {
	hasMore: boolean;
	nextCursor: number | null;
	totalMessages: number | null;
	loadingOlder: boolean;
}

export interface HistoryPayload {
	historyCacheHit?: boolean;
	history?: HistoryMessage[];
	historyTruncated?: boolean;
	historyDroppedCount?: number;
	historyOmitted?: boolean;
	hasMore?: boolean;
	nextCursor?: number;
	totalMessages?: number;
}

/** HTTP error response from the sessions API. */
interface HttpErrorPayload {
	error?: string;
}

/** History message with optional created_at and seq fields for outgoing user messages. */
interface OutgoingUserMessage extends HistoryMessage {
	created_at?: number;
	seq?: number | null;
}

export const SESSION_HISTORY_PAGE_LIMIT = 120;
const sessionHistoryPaging = new Map<string, HistoryPaginationState>();

/** Whether an assistant frame renders a bubble of its own. */
function assistantRendersBubble(message: HistoryMessage): boolean {
	if (typeof message.content === "string" && message.content.trim()) return true;
	if (typeof message.audio === "string" && message.audio.trim()) return true;
	return isReasoningContent(message.reasoning) && hasVisibleReasoning(message.reasoning);
}

/** Segment identifiers that already have an assistant message of their own. */
function assistantSegmentIds(history: HistoryMessage[]): Set<string> {
	const ids = new Set<string>();
	for (const message of history) {
		if (message?.role !== "assistant") continue;
		const id = message.segmentId || message.segment_id;
		if (typeof id === "string" && id) ids.add(id);
	}
	return ids;
}

function segmentIdOfUpdate(message: HistoryMessage): string | null {
	const direct = message.segmentId;
	if (typeof direct === "string" && direct) return direct;
	const update = message.update as { segmentId?: string } | undefined;
	const nested = update?.segmentId;
	return typeof nested === "string" && nested ? nested : null;
}

/// Roles that always render a bubble of their own.
const ALWAYS_RENDERED_ROLES = new Set(["user", "system", "notice", "checkpoint", "tool_lifecycle"]);

/**
 * Whether each record of `history` renders a bubble of its own.
 *
 * Mirrors `rendered_bubble_flags` in `chelix-sessions`. It cannot be decided per
 * record in isolation: a provider segment renders one bubble however many
 * records it spans, and none once an assistant message carries that segment in
 * its final form.
 */
function renderedBubbleFlags(history: HistoryMessage[]): boolean[] {
	const assistantSegments = assistantSegmentIds(history);
	const renderedSegments = new Set<string>();
	return history.map((message) => {
		const role = message?.role;
		if (role && ALWAYS_RENDERED_ROLES.has(role)) return true;
		if (role === "assistant") return assistantRendersBubble(message);
		if (role !== "provider_update") return false;
		const id = segmentIdOfUpdate(message);
		if (!id || assistantSegments.has(id) || renderedSegments.has(id)) return false;
		renderedSegments.add(id);
		return true;
	});
}

/** Number of bubbles `history` renders as. */
export function countDisplayableMessages(history: HistoryMessage[]): number {
	if (!Array.isArray(history)) return 0;
	return renderedBubbleFlags(history).filter(Boolean).length;
}

/**
 * Whether appending `message` to `history` adds a bubble.
 *
 * The live counter is incremented per arriving record, so it must ask the same
 * question the totals answer: an assistant frame carrying only tool calls, or a
 * record of a segment that is already on screen, adds nothing.
 */
export function appendingAddsBubble(history: HistoryMessage[], message: HistoryMessage): boolean {
	const flags = renderedBubbleFlags([...(Array.isArray(history) ? history : []), message]);
	return flags[flags.length - 1] === true;
}

function toValidHistoryIndex(value: unknown): number | null {
	if (value === null || value === undefined) return null;
	const idx = Number(value);
	if (!Number.isInteger(idx) || idx < 0) return null;
	return idx;
}

export function clearHistoryPaginationState(key?: string): void {
	if (key === undefined) {
		sessionHistoryPaging.clear();
		return;
	}
	if (!key) return;
	sessionHistoryPaging.delete(key);
}

export function setHistoryPaginationState(key: string, payload: HistoryPayload): void {
	if (!key) return;
	const hasMore = payload?.hasMore === true;
	const nextCursor = toValidHistoryIndex(payload?.nextCursor);
	const totalMessages = Number(payload?.totalMessages);
	sessionHistoryPaging.set(key, {
		hasMore: hasMore && nextCursor !== null,
		nextCursor: hasMore ? nextCursor : null,
		totalMessages: Number.isInteger(totalMessages) && totalMessages >= 0 ? totalMessages : null,
		loadingOlder: false,
	});
}

export function getHistoryPaginationState(key: string): HistoryPaginationState | null {
	return sessionHistoryPaging.get(key) || null;
}

export function setHistoryPaginationLoading(key: string, loadingOlder: boolean): HistoryPaginationState | null {
	const paging = getHistoryPaginationState(key);
	if (!paging) return null;
	const next = { ...paging, loadingOlder };
	sessionHistoryPaging.set(key, next);
	return next;
}

export function isHistoryCacheComplete(key: string): boolean {
	const paging = getHistoryPaginationState(key);
	return !paging || paging.hasMore !== true;
}

export function historyIndexFromMessage(message: HistoryMessage | null | undefined): number | null {
	if (!(message && typeof message === "object")) return null;
	const idx = toValidHistoryIndex(message.historyIndex);
	if (idx !== null) return idx;
	return toValidHistoryIndex(message.messageIndex);
}

export function computeHistoryTailIndex(history: HistoryMessage[]): number {
	let max = -1;
	if (!Array.isArray(history)) return max;
	for (let i = 0; i < history.length; i += 1) {
		const indexed = historyIndexFromMessage(history[i]);
		if (indexed !== null) {
			if (indexed > max) max = indexed;
			continue;
		}
		if (i > max) max = i;
	}
	return max;
}

export function historyHasUnindexedMessages(history: HistoryMessage[]): boolean {
	if (!Array.isArray(history)) return false;
	for (const msg of history) {
		if (historyIndexFromMessage(msg) === null) return true;
	}
	return false;
}

/**
 * Index the next appended record will receive.
 *
 * Derived from the observed history tail, never from message counters: those
 * count displayable messages, while a record index addresses a stored record
 * and a single message can span many of them.
 */
function nextSessionHistoryIndex(key: string): number | null {
	const session = sessionStore.getByKey(key);
	if (session && session.lastHistoryIndex.value >= 0) return session.lastHistoryIndex.value + 1;
	if (key === S.activeSessionKey && S.lastHistoryIndex >= 0) return S.lastHistoryIndex + 1;
	return null;
}

export function cacheSessionHistoryMessage(key: string, message: HistoryMessage, historyIndex?: number): boolean {
	return upsertSessionHistoryMessage(key, message, historyIndex);
}

export function cacheOutgoingUserMessage(key: string, chatParams: ChatParams): void {
	if (!(key && chatParams)) return;
	const historyIndex = nextSessionHistoryIndex(key);
	const next: OutgoingUserMessage = {
		role: "user",
		content: (chatParams.content && Array.isArray(chatParams.content)
			? chatParams.content
			: chatParams.text || "") as string,
		created_at: Date.now(),
		seq: chatParams._seq || null,
	};
	if (historyIndex !== null) next.historyIndex = historyIndex;
	upsertSessionHistoryMessage(key, next, historyIndex ?? undefined);
}

export function clearSessionHistoryCache(key?: string): void {
	clearSessionHistory(key);
	clearHistoryPaginationState(key);
}

export async function fetchSessionHistoryViaHttp(
	key: string,
	options?: { cachedMessageCount?: number; cursor?: number; limit?: number },
): Promise<HistoryPayload> {
	const opts = options || {};
	const query = new URLSearchParams();
	if (Number.isInteger(opts.cachedMessageCount) && (opts.cachedMessageCount as number) >= 0) {
		query.set("cached_message_count", String(opts.cachedMessageCount));
	}
	if (Number.isInteger(opts.cursor) && (opts.cursor as number) >= 0) {
		query.set("cursor", String(opts.cursor));
	}
	if (Number.isInteger(opts.limit) && (opts.limit as number) > 0) {
		query.set("limit", String(opts.limit));
	}
	let url = `/api/sessions/${encodeURIComponent(key)}/history`;
	const qs = query.toString();
	if (qs) url += `?${qs}`;

	const response = await fetch(url, {
		headers: { Accept: "application/json" },
	});
	let payload: HistoryPayload | null = null;
	try {
		payload = await response.json();
	} catch {
		payload = null;
	}
	if (!response.ok) {
		const errMsg = (payload as HttpErrorPayload | null)?.error || `Failed to load session history (${response.status})`;
		throw new Error(errMsg);
	}
	return payload || {};
}

export function mergeHistoryPages(existingHistory: HistoryMessage[], olderHistory: HistoryMessage[]): HistoryMessage[] {
	const older = Array.isArray(olderHistory) ? olderHistory : [];
	const current = Array.isArray(existingHistory) ? existingHistory : [];
	if (older.length === 0) return current;
	if (current.length === 0) return older;

	const byIndex = new Map<number, HistoryMessage>();
	const ordered: HistoryMessage[] = [];
	const pushMessage = (msg: HistoryMessage): void => {
		const idx = historyIndexFromMessage(msg);
		if (idx === null) {
			ordered.push(msg);
			return;
		}
		if (!byIndex.has(idx)) {
			ordered.push(msg);
		}
		byIndex.set(idx, msg);
	};

	for (const olderMsg of older) pushMessage(olderMsg);
	for (const currentMsg of current) pushMessage(currentMsg);

	return ordered.map((msg) => {
		const idx = historyIndexFromMessage(msg);
		if (idx === null) return msg;
		return byIndex.get(idx) || msg;
	});
}

export function shouldApplyServerHistory(
	key: string,
	serverHistory: HistoryMessage[],
	requestRevision: number,
): boolean {
	const current = getSessionHistory(key);
	if (!current) return true;
	const serverTail = computeHistoryTailIndex(serverHistory);
	const currentTail = computeHistoryTailIndex(current);
	if (serverTail > currentTail) return true;
	if (serverTail < currentTail) return false;
	const currentRevision = getHistoryRevision(key);
	if (currentRevision === requestRevision) return true;
	return !historyHasUnindexedMessages(current);
}

export function syncHistoryState(
	key: string,
	history: HistoryMessage[],
	historyTailIndex: number,
	totalCountHint: number | null,
): void {
	const loadedCount = countDisplayableMessages(history);
	const sessionEntry = sessionStore.getByKey(key);
	const legacy = (S.sessions as SessionMeta[]).find((session) => session.key === key);
	const existingCount = Number.isInteger(sessionEntry?.messageCount) ? (sessionEntry?.messageCount as number) : 0;
	const legacyCount = Number.isInteger(legacy?.messageCount) ? (legacy?.messageCount as number) : 0;
	const hintedCount = Number.isInteger(totalCountHint) ? (totalCountHint as number) : 0;
	const count = Math.max(loadedCount, existingCount, hintedCount, legacyCount);
	if (sessionEntry) {
		sessionEntry.syncCounts(count, count);
		sessionEntry.localUnread.value = false;
		sessionEntry.lastHistoryIndex.value = historyTailIndex;
	}
	if (legacy) {
		legacy.messageCount = count;
		legacy.lastSeenMessageCount = count;
		legacy._localUnread = false;
	}
	S.setLastHistoryIndex(historyTailIndex);
}
