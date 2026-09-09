import type { UiHistoryBatch, UiHistoryPage, UiSnapshot } from "../types/ui-history";

interface HistoryWindow {
	generation: string;
	revision: number;
	totalMessages: number;
	history: UiSnapshot[];
	hasOlder: boolean;
	hasNewer: boolean;
}

const windows = new Map<string, HistoryWindow>();
export const MAX_WINDOW_MESSAGES = 240;
const MAX_WINDOW_BYTES = 12 * 1024 * 1024;
let activeKey = "";

function trimWindow(window: HistoryWindow, keep: "older" | "newer"): void {
	const sizes = window.history.map((message) => new TextEncoder().encode(JSON.stringify(message)).length);
	let bytes = sizes.reduce((sum, size) => sum + size, 0);
	while (window.history.length > 1 && (window.history.length > MAX_WINDOW_MESSAGES || bytes > MAX_WINDOW_BYTES)) {
		if (keep === "older") {
			window.history.pop();
			bytes -= sizes.pop() || 0;
			window.hasNewer = true;
		} else {
			window.history.shift();
			bytes -= sizes.shift() || 0;
			window.hasOlder = true;
		}
	}
}

function merge(messages: UiSnapshot[], updates: UiSnapshot[], generation: string): UiSnapshot[] {
	const byId = new Map(messages.map((message) => [message.id, message]));
	for (const update of updates) {
		const previous = byId.get(update.id);
		if (!previous || previous.revision < update.revision) byId.set(update.id, { ...update, generation });
	}
	return [...byId.values()].sort((left, right) => left.position - right.position);
}

export function applyHistoryPage(
	key: string,
	page: UiHistoryPage,
	direction: "replace" | "older" | "newer" = "replace",
): boolean {
	const current = windows.get(key);
	if (direction !== "replace" && current?.generation !== page.generation) return false;
	const sameGeneration = current?.generation === page.generation;
	const baseline = direction === "replace" ? [] : current?.history || [];
	const pageIds = new Set(page.history.map((message) => message.id));
	const later = sameGeneration
		? (current?.history || []).filter(
				(message) => message.revision > page.revision && (direction !== "replace" || pageIds.has(message.id)),
			)
		: [];
	const next: HistoryWindow = {
		generation: page.generation,
		revision: Math.max(page.revision, sameGeneration ? current.revision : 0),
		totalMessages: sameGeneration && current.revision > page.revision ? current.totalMessages : page.totalMessages,
		history: merge(merge(baseline, page.history, page.generation), later, page.generation),
		hasOlder: direction === "newer" ? current?.hasOlder === true : page.hasOlder,
		hasNewer: direction === "older" ? current?.hasNewer === true : page.hasNewer,
	};
	trimWindow(next, direction === "older" ? "older" : "newer");
	windows.set(key, next);
	return true;
}

export function applyHistoryBatch(key: string, batch: UiHistoryBatch): boolean {
	const current = windows.get(key);
	if (!current || current.generation !== batch.generation || batch.revision <= current.revision) return false;
	if (batch.fromRevision > current.revision) throw new Error("UI history revision gap");
	const last = current.history.at(-1)?.position;
	const loaded = new Set(current.history.map((message) => message.id));
	const updates = batch.history.filter(
		(message) => loaded.has(message.id) || (!current.hasNewer && (last === undefined || message.position > last)),
	);
	current.history = merge(current.history, updates, current.generation);
	current.revision = batch.revision;
	current.totalMessages = batch.totalMessages;
	trimWindow(current, "newer");
	return true;
}

export function getHistoryWindow(key: string): Readonly<HistoryWindow> | null {
	return windows.get(key) || null;
}

export function getSessionHistory(key: string): UiSnapshot[] | null {
	return windows.get(key)?.history || null;
}

export function getHistoryRevision(key: string): number {
	return windows.get(key)?.revision || 0;
}

export function hasSessionHistory(key: string): boolean {
	return windows.has(key);
}

export function retainSessionHistory(key: string): void {
	activeKey = key;
	for (const candidate of windows.keys()) if (candidate !== activeKey) windows.delete(candidate);
}

export function clearSessionHistory(key?: string): void {
	if (key === undefined) windows.clear();
	else windows.delete(key);
}
