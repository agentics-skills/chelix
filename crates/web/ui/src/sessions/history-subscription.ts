import { updateTokenBar } from "../chat-ui";
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
import { isToolLifecycleEvent } from "../tool-lifecycle";
import type { UiHistoryBatch, UiHistoryEvent, UiHistoryPage, UiHistoryRange, UiSnapshot } from "../types/ui-history";
import type { ContextBudgetMetadata } from "../types/ws-events";
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
	bufferedContextBudget?: BufferedContextBudget;
}
let subscription: Subscription | null = null;
let sequence = 0;

interface ContextBudgetSource {
	position: number;
	revision: number;
	contextBudget: ContextBudgetMetadata;
}

interface BufferedContextBudget {
	generation: string;
	source: ContextBudgetSource;
}

interface ContextBudgetCursor {
	sessionKey: string;
	generation: string;
	source: ContextBudgetSource | null;
}

let contextBudgetCursor: ContextBudgetCursor | null = null;

function contextBudgetFollows(candidate: ContextBudgetSource, current: ContextBudgetSource): boolean {
	return (
		candidate.position > current.position ||
		(candidate.position === current.position && candidate.revision >= current.revision)
	);
}

function latestContextBudget(history: UiSnapshot[]): ContextBudgetSource | null {
	let latest: ContextBudgetSource | null = null;
	for (const message of history) {
		if (!(isToolLifecycleEvent(message) && message.contextBudget)) continue;
		const candidate: ContextBudgetSource = {
			position: message.position,
			revision: message.revision,
			contextBudget: message.contextBudget,
		};
		if (!latest || contextBudgetFollows(candidate, latest)) latest = candidate;
	}
	return latest;
}

function applyContextBudgetSource(
	key: string,
	generation: string,
	candidate: ContextBudgetSource | null,
	reapplyCurrent: boolean,
): void {
	const current = contextBudgetCursor;
	if (!current || current.sessionKey !== key || current.generation !== generation) {
		contextBudgetCursor = { sessionKey: key, generation, source: candidate };
		updateTokenBar(candidate?.contextBudget ?? null);
		return;
	}
	if (candidate && (!current.source || contextBudgetFollows(candidate, current.source))) {
		contextBudgetCursor = { sessionKey: key, generation, source: candidate };
		updateTokenBar(candidate.contextBudget);
		return;
	}
	if (reapplyCurrent) updateTokenBar(current.source?.contextBudget ?? null);
}

function applyAcceptedContextBudget(
	key: string,
	generation: string,
	history: UiSnapshot[],
	reapplyCurrent: boolean,
	acceptHistorySource: boolean,
): void {
	applyContextBudgetSource(key, generation, acceptHistorySource ? latestContextBudget(history) : null, reapplyCurrent);
}

function rememberBufferedContextBudget(current: Subscription, event: UiHistoryEvent): void {
	if (event.snapshot && current.bufferedContextBudget?.generation !== event.snapshot.generation) {
		current.bufferedContextBudget = undefined;
	}
	if (!event.update) return;
	if (current.bufferedContextBudget?.generation !== event.update.generation) {
		current.bufferedContextBudget = undefined;
	}
	const source = latestContextBudget(event.update.history);
	const previous = current.bufferedContextBudget;
	if (source && (!previous || contextBudgetFollows(source, previous.source))) {
		current.bufferedContextBudget = { generation: event.update.generation, source };
	}
}

export function invalidateHistorySubscription(): void {
	subscription = null;
}

function buffer(current: Subscription, event: UiHistoryEvent): void {
	rememberBufferedContextBudget(current, event);
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

function bufferBeforeReady(current: Subscription, event: UiHistoryEvent): void {
	buffer(current, event);
	const count = current.buffered?.snapshot?.history.length ?? current.buffered?.update?.history.length ?? 0;
	if (count <= MAX_WINDOW_MESSAGES) return;
	current.buffered = undefined;
	current.needsSnapshot = true;
}

function applyHistoryEvent(key: string, event: UiHistoryEvent): boolean {
	if (event.snapshot) return applyHistoryPage(key, event.snapshot);
	if (event.update) return applyHistoryBatch(key, event.update);
	return false;
}

function applyEventContextBudget(
	key: string,
	event: UiHistoryEvent,
	bufferedContextBudget: Subscription["bufferedContextBudget"],
): void {
	if (event.snapshot) {
		applyAcceptedContextBudget(key, event.snapshot.generation, event.snapshot.history, true, !event.snapshot.hasNewer);
	} else if (event.update) {
		applyAcceptedContextBudget(key, event.update.generation, event.update.history, false, true);
	}
	const generation = event.snapshot?.generation ?? event.update?.generation;
	if (bufferedContextBudget && bufferedContextBudget.generation === generation) {
		applyContextBudgetSource(key, bufferedContextBudget.generation, bufferedContextBudget.source, false);
	}
}

function recoverHistorySubscription(current: Subscription, error: unknown): void {
	invalidateHistorySubscription();
	showToast(error instanceof Error ? error.message : "UI history synchronization failed", "error");
	void subscribeSessionHistory(current.key, undefined, current.range)
		.then(() => reconcileSessionHistory(current.key))
		.catch(reportSubscriptionError);
}

function accept(event: UiHistoryEvent): void {
	const current = subscription;
	if (!current || event.subscriptionId !== current.id || event.sessionKey !== current.key) return;
	if (!current.ready) {
		bufferBeforeReady(current, event);
		return;
	}
	const bufferedContextBudget = current.bufferedContextBudget;
	current.bufferedContextBudget = undefined;
	try {
		if (!applyHistoryEvent(current.key, event)) return;
		applyEventContextBudget(current.key, event, bufferedContextBudget);
		syncHistoryState(current.key);
		reconcileSessionHistory(current.key);
	} catch (error) {
		recoverHistorySubscription(current, error);
	}
}

onEvent("ui_history", (payload) => accept(payload as UiHistoryEvent));

function reportSubscriptionError(error: unknown): void {
	showToast(error instanceof Error ? error.message : "UI history subscription failed", "error");
}

function completeHistoryBaseline(current: Subscription, key: string, generation: string): void {
	current.ready = true;
	const buffered = current.buffered;
	const pendingContextBudget = current.bufferedContextBudget;
	current.buffered = undefined;
	if (buffered) {
		accept(buffered);
		return;
	}
	current.bufferedContextBudget = undefined;
	if (pendingContextBudget?.generation === generation) {
		applyContextBudgetSource(key, pendingContextBudget.generation, pendingContextBudget.source, false);
	}
}

export function subscribeSessionHistory(
	key: string,
	around?: string,
	selectedRange?: UiHistoryRange,
): Promise<UiHistoryPage | null> {
	return subscribeSessionHistoryRequest(key, around, selectedRange);
}

async function subscribeSessionHistoryRequest(
	key: string,
	around?: string,
	selectedRange?: UiHistoryRange,
	preservedContextBudget?: BufferedContextBudget,
): Promise<UiHistoryPage | null> {
	const range: UiHistoryRange =
		selectedRange ?? (around ? { direction: "around", message_id: around } : { direction: "latest" });
	const current: Subscription = {
		id: crypto.randomUUID(),
		key,
		ready: false,
		range,
		bufferedContextBudget: preservedContextBudget,
	};
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
	if (current.needsSnapshot) {
		return subscribeSessionHistoryRequest(key, undefined, range, current.bufferedContextBudget);
	}
	retainSessionHistory(key);
	applyHistoryPage(key, response.payload.snapshot);
	applyAcceptedContextBudget(
		key,
		response.payload.snapshot.generation,
		response.payload.snapshot.history,
		true,
		!response.payload.snapshot.hasNewer,
	);
	completeHistoryBaseline(current, key, response.payload.snapshot.generation);
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
