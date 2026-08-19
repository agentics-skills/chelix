// ── Session history cache (in-memory) ─────────────────────────
//
// Stores per-session chat history so re-selecting a session can render
// immediately. Histories are patched incrementally from websocket events and
// refreshed authoritatively from sessions.switch responses.

import type { HistoryMessage } from "../types/session";

const historyByKey = new Map<string, HistoryMessage[]>();
const revisionByKey = new Map<string, number>();
const bytesByKey = new Map<string, number>();
const lastAccessByKey = new Map<string, number>();
/// Session whose history must be kept whole.
///
/// The user scrolls through this one, so its pages are on screen and the cache
/// is their only copy: the server pages strictly below the cursor and reports
/// `hasMore: false` at the first message, so a dropped page can never be
/// fetched again. Inactive sessions carry no such constraint — they are dropped
/// whole and reloaded on the next switch.
let retainedHistoryKey: string | null = null;
let totalBytes = 0;
const encoder: TextEncoder | null = typeof TextEncoder === "function" ? new TextEncoder() : null;

const MAX_TOTAL_HISTORY_BYTES = 12 * 1024 * 1024;
/// How many sessions stay cached. Switching between a handful of chats stays
/// instant, while a long browsing streak cannot accumulate every chat visited.
const MAX_CACHED_SESSIONS = 3;

function deepClone<T>(value: T): T {
	if (value === undefined) return undefined as T;
	if (typeof structuredClone === "function") {
		try {
			return structuredClone(value);
		} catch (_e) {
			// Fall through to JSON clone.
		}
	}
	return JSON.parse(JSON.stringify(value));
}

function toValidIndex(value: unknown): number | null {
	if (value === null || value === undefined) return null;
	const parsed = Number(value);
	if (!Number.isInteger(parsed) || parsed < 0) return null;
	return parsed;
}

function messageHistoryIndex(msg: HistoryMessage | null | undefined): number | null {
	if (!(msg && typeof msg === "object")) return null;
	const direct = toValidIndex(msg.historyIndex);
	if (direct !== null) return direct;
	return toValidIndex(msg.messageIndex);
}

function bumpRevision(key: string): void {
	revisionByKey.set(key, (revisionByKey.get(key) || 0) + 1);
}

function touchHistoryKey(key: string): void {
	lastAccessByKey.set(key, Date.now());
}

/// Size of a single cached message.
///
/// Sizing is per message and never over the whole list: a streamed answer
/// appends thousands of records, and re-serializing the history on each one
/// costs quadratic time on the main thread, which is felt as the chat freezing
/// while a response streams in.
function estimateMessageBytes(message: HistoryMessage): number {
	const serialized = JSON.stringify(message ?? null);
	if (!serialized) return 0;
	return encoder ? encoder.encode(serialized).length : serialized.length;
}

function estimateHistoryBytes(history: HistoryMessage[]): number {
	let bytes = 0;
	for (const message of history) bytes += estimateMessageBytes(message);
	return bytes;
}

function updateHistorySize(key: string, nextBytes: number): void {
	const prev = bytesByKey.get(key) || 0;
	bytesByKey.set(key, nextBytes);
	totalBytes += nextBytes - prev;
	if (totalBytes < 0) totalBytes = 0;
}

function dropHistoryKey(key: string): void {
	const prev = bytesByKey.get(key) || 0;
	historyByKey.delete(key);
	revisionByKey.delete(key);
	bytesByKey.delete(key);
	lastAccessByKey.delete(key);
	if (retainedHistoryKey === key) retainedHistoryKey = null;
	totalBytes -= prev;
	if (totalBytes < 0) totalBytes = 0;
}

function oldestHistoryKey(preferredKey: string): string | null {
	let victim: string | null = null;
	let oldest = Number.POSITIVE_INFINITY;
	for (const [key, ts] of lastAccessByKey.entries()) {
		if (key === preferredKey || key === retainedHistoryKey) continue;
		if (ts < oldest) {
			oldest = ts;
			victim = key;
		}
	}
	return victim;
}

function evictGlobalHistoryBudget(preferredKey: string): void {
	while (totalBytes > MAX_TOTAL_HISTORY_BYTES || historyByKey.size > MAX_CACHED_SESSIONS) {
		const victim = oldestHistoryKey(preferredKey);
		if (!victim) break;
		dropHistoryKey(victim);
	}
}

/// A session is kept whole or dropped whole. Cutting the head off a cached
/// history leaves a session that looks complete but is not: the cache is the
/// only copy of the pages already fetched, and the server pages strictly below
/// the cursor, so the removed messages could never be loaded again.
function enforceHistoryBudgets(key: string, addedBytes: number): void {
	if (!historyByKey.has(key)) return;
	updateHistorySize(key, (bytesByKey.get(key) || 0) + addedBytes);
	touchHistoryKey(key);
	evictGlobalHistoryBudget(key);
}

function normalizeMessage(message: unknown, fallbackIndex?: number | null): HistoryMessage {
	let next: HistoryMessage = (deepClone(message) as HistoryMessage) || {};
	if (!(next && typeof next === "object")) {
		next = { role: "notice", content: String(message || "") };
	}
	const idx = toValidIndex(fallbackIndex);
	const msgIdx = idx === null ? messageHistoryIndex(next) : idx;
	if (msgIdx !== null) next.historyIndex = msgIdx;
	return next;
}

/// Outcome of an upsert: whether a message was added, and how many bytes the
/// message it replaced occupied.
interface UpsertOutcome {
	inserted: boolean;
	replacedBytes: number;
}

function replaceAt(list: HistoryMessage[], index: number, next: HistoryMessage): UpsertOutcome {
	const replacedBytes = estimateMessageBytes(list[index]);
	list[index] = next;
	return { inserted: false, replacedBytes };
}

function upsertWithoutIndex(list: HistoryMessage[], next: HistoryMessage): UpsertOutcome {
	if (next.role === "tool_lifecycle" && typeof next.toolCallId === "string") {
		const existingLifecycleIndex = list.findIndex(
			(message) =>
				message?.role === "tool_lifecycle" && message.toolCallId === next.toolCallId && message.runId === next.runId,
		);
		if (existingLifecycleIndex >= 0) {
			return replaceAt(list, existingLifecycleIndex, next);
		}
	}
	if (next.role === "assistant" && (next.segmentId || next.segment_id)) {
		const targetSegmentId = next.segmentId || next.segment_id;
		const existingSegIdx = list.findIndex(
			(msg) => msg?.role === "assistant" && (msg.segmentId === targetSegmentId || msg.segment_id === targetSegmentId),
		);
		if (existingSegIdx >= 0) {
			return replaceAt(list, existingSegIdx, next);
		}
	}
	if (next.role === "provider_update" && next.update && typeof next.update === "object") {
		const u = next.update as { segmentId?: string; itemId?: string; updateSeq?: number };
		const existingUpdateIdx = list.findIndex((message) => {
			if (message?.role !== "provider_update" || !message.update || typeof message.update !== "object") return false;
			const other = message.update as { segmentId?: string; itemId?: string; updateSeq?: number };
			return other.segmentId === u.segmentId && other.itemId === u.itemId && other.updateSeq === u.updateSeq;
		});
		if (existingUpdateIdx >= 0) {
			return replaceAt(list, existingUpdateIdx, next);
		}
	}
	list.push(next);
	return { inserted: true, replacedBytes: 0 };
}

function upsertByIndex(list: HistoryMessage[], next: HistoryMessage, historyIndex: number): UpsertOutcome {
	const existingIdx = list.findIndex((msg) => messageHistoryIndex(msg) === historyIndex);
	if (existingIdx >= 0) {
		return replaceAt(list, existingIdx, next);
	}
	const insertAt = list.findIndex((msg) => {
		const other = messageHistoryIndex(msg);
		if (other === null) return true;
		return other > historyIndex;
	});
	if (insertAt === -1) {
		list.push(next);
		return { inserted: true, replacedBytes: 0 };
	}
	list.splice(insertAt, 0, next);
	return { inserted: true, replacedBytes: 0 };
}

export function getHistoryRevision(key: string): number {
	return revisionByKey.get(key) || 0;
}

/// Keep `key` whole and let every other session be evicted.
///
/// Called when a session becomes the one on screen. Only one session can be
/// retained: the previous one is released and falls back under the normal
/// budget, so browsing many chats cannot pile them all up in memory.
export function retainSessionHistory(key: string): void {
	if (retainedHistoryKey === key) return;
	retainedHistoryKey = key;
	enforceHistoryBudgets(key, 0);
}

export function hasSessionHistory(key: string): boolean {
	return historyByKey.has(key);
}

export function getSessionHistory(key: string): HistoryMessage[] | null {
	const history = historyByKey.get(key) || null;
	if (history) touchHistoryKey(key);
	return history;
}

export function replaceSessionHistory(key: string, history: unknown[]): HistoryMessage[] {
	const next = Array.isArray(history) ? history.map((msg) => normalizeMessage(msg)) : [];
	historyByKey.set(key, next);
	updateHistorySize(key, estimateHistoryBytes(next));
	bumpRevision(key);
	enforceHistoryBudgets(key, 0);
	return next;
}

export function upsertSessionHistoryMessage(key: string, message: unknown, historyIndex?: number | null): boolean {
	let list = historyByKey.get(key);
	if (!list) {
		list = [];
		historyByKey.set(key, list);
	}
	const next = normalizeMessage(message, historyIndex);
	const idx = messageHistoryIndex(next);
	const outcome = idx !== null ? upsertByIndex(list, next, idx) : upsertWithoutIndex(list, next);
	bumpRevision(key);
	enforceHistoryBudgets(key, estimateMessageBytes(next) - outcome.replacedBytes);
	return outcome.inserted;
}

export function clearSessionHistory(key?: string): void {
	if (key === undefined) {
		historyByKey.clear();
		revisionByKey.clear();
		bytesByKey.clear();
		lastAccessByKey.clear();
		retainedHistoryKey = null;
		totalBytes = 0;
		return;
	}
	dropHistoryKey(key);
}
