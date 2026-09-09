import { onEvent } from "../events";
import { sendRpc } from "../helpers";
import * as S from "../state";
import {
	applyHistoryBatch,
	applyHistoryPage,
	getHistoryWindow,
	getSessionHistory,
	MAX_WINDOW_MESSAGES,
	retainSessionHistory,
} from "../stores/session-history-cache";
import type { UiHistoryBatch, UiHistoryEvent, UiHistoryPage, UiHistoryRange } from "../types/ui-history";
import { showToast } from "../ui";
import { SESSION_HISTORY_PAGE_LIMIT, syncHistoryState } from "./session-history";
import { reconcileSessionHistory } from "./session-render";

interface Subscription {
	id: string;
	key: string;
	ready: boolean;
	range: UiHistoryRange;
	needsSnapshot?: boolean;
	buffered?: UiHistoryEvent;
}
let subscription: Subscription | null = null;
let sequence = 0;

export function invalidateHistorySubscription(): void {
	subscription = null;
}

function buffer(current: Subscription, event: UiHistoryEvent): void {
	if (current.needsSnapshot) return;
	const previous = current.buffered;
	if (event.snapshot || !previous) {
		current.buffered = event;
		return;
	}
	if (!event.update) return;
	if (previous.snapshot && previous.snapshot.generation === event.update.generation) {
		const history = new Map(previous.snapshot.history.map((message) => [message.id, message]));
		for (const message of event.update.history) history.set(message.id, message);
		previous.snapshot.history = [...history.values()].sort((left, right) => left.position - right.position);
		previous.snapshot.revision = event.update.revision;
		previous.snapshot.totalMessages = event.update.totalMessages;
		return;
	}
	if (!previous.update || previous.update.generation !== event.update.generation) {
		current.buffered = event;
		return;
	}
	const history = new Map(previous.update.history.map((message) => [message.id, message]));
	for (const message of event.update.history) history.set(message.id, message);
	const update: UiHistoryBatch = {
		...event.update,
		fromRevision: previous.update.fromRevision,
		history: [...history.values()],
	};
	current.buffered = { ...event, update };
}

function accept(event: UiHistoryEvent): void {
	const current = subscription;
	if (!current || event.subscriptionId !== current.id || event.sessionKey !== current.key) return;
	if (!current.ready) {
		buffer(current, event);
		const count = current.buffered?.snapshot?.history.length ?? current.buffered?.update?.history.length ?? 0;
		if (count > MAX_WINDOW_MESSAGES) {
			current.buffered = undefined;
			current.needsSnapshot = true;
		}
		return;
	}
	try {
		const changed = event.snapshot
			? applyHistoryPage(current.key, event.snapshot)
			: event.update
				? applyHistoryBatch(current.key, event.update)
				: false;
		if (!changed) return;
		syncHistoryState(current.key);
		reconcileSessionHistory(current.key);
	} catch (error) {
		invalidateHistorySubscription();
		showToast(error instanceof Error ? error.message : "UI history synchronization failed", "error");
		void subscribeSessionHistory(current.key, undefined, current.range)
			.then(() => reconcileSessionHistory(current.key))
			.catch(reportSubscriptionError);
	}
}

onEvent("ui_history", (payload) => accept(payload as UiHistoryEvent));

function reportSubscriptionError(error: unknown): void {
	showToast(error instanceof Error ? error.message : "UI history subscription failed", "error");
}

export async function subscribeSessionHistory(
	key: string,
	around?: string,
	selectedRange?: UiHistoryRange,
): Promise<UiHistoryPage | null> {
	const range: UiHistoryRange =
		selectedRange ?? (around ? { direction: "around", message_id: around } : { direction: "latest" });
	const current: Subscription = { id: crypto.randomUUID(), key, ready: false, range };
	subscription = current;
	const response = await sendRpc<{ subscriptionId: string; sessionKey: string; snapshot: UiHistoryPage }>(
		"sessions.history.subscribe",
		{
			key,
			subscriptionId: current.id,
			sequence: ++sequence,
			limit: selectedRange ? MAX_WINDOW_MESSAGES : SESSION_HISTORY_PAGE_LIMIT,
			range,
		},
	);
	if (subscription !== current || S.activeSessionKey !== key) return null;
	if (!(response.ok && response.payload)) throw new Error(response.error?.message || "History subscription failed");
	if (response.payload.subscriptionId !== current.id || response.payload.sessionKey !== key)
		throw new Error("History subscription identity mismatch");
	if (current.needsSnapshot) return subscribeSessionHistory(key, undefined, range);
	retainSessionHistory(key);
	applyHistoryPage(key, response.payload.snapshot);
	current.ready = true;
	if (current.buffered) accept(current.buffered);
	current.buffered = undefined;
	syncHistoryState(key);
	if (!getSessionHistory(key)) throw new Error("History baseline missing");
	return response.payload.snapshot;
}

export async function subscribeHistoryWindow(key: string): Promise<void> {
	const window = getHistoryWindow(key);
	const first = window?.history[0];
	const last = window?.history.at(-1);
	if (!(window && first && last)) return;
	const page = await subscribeSessionHistory(key, undefined, {
		direction: "window",
		start: first.position,
		end: window.hasNewer ? last.position + 1 : null,
	});
	if (page && key === S.activeSessionKey) reconcileSessionHistory(key);
}
