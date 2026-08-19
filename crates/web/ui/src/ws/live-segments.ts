// ── Live provider segment registry ────────────────────────────
//
// One reducer per session holds the canonical provider items of the segment
// currently streaming. Both the chat handlers and the tool card renderer read
// positions from here, so live rendering orders nodes exactly like reload does.

import { createProviderSegmentViewModel, type ProviderSegmentViewModel } from "../sessions/provider-segment-reducer";

const liveSegments = new Map<string, ProviderSegmentViewModel>();

/// Reducer for `segmentId` in `sessionKey`, opening a fresh one when the
/// provider started a new segment.
export function liveSegmentFor(sessionKey: string, segmentId: string): ProviderSegmentViewModel {
	const existing = liveSegments.get(sessionKey);
	if (existing && existing.segmentId === segmentId) return existing;
	const created = createProviderSegmentViewModel(segmentId);
	liveSegments.set(sessionKey, created);
	return created;
}

/// Open a new segment for `sessionKey`, discarding any previous one.
export function openLiveSegment(sessionKey: string, segmentId: string): ProviderSegmentViewModel {
	const created = createProviderSegmentViewModel(segmentId);
	liveSegments.set(sessionKey, created);
	return created;
}

/// Segment currently streaming in `sessionKey`, if any.
export function currentLiveSegment(sessionKey: string): ProviderSegmentViewModel | undefined {
	return liveSegments.get(sessionKey);
}

/// Canonical position of the provider item identified by `itemId`.
///
/// Returns `null` when the segment has not announced that item yet. Callers
/// must not substitute a position of their own.
export function liveItemPosition(sessionKey: string, itemId: string): number | null {
	const segment = liveSegments.get(sessionKey);
	if (!segment) return null;
	const item = segment.items.find((candidate) => candidate.id === itemId);
	return item ? item.position : null;
}

/// Canonical position of the function call whose provider call id is `callId`.
///
/// Tool lifecycle events carry the provider call id, while the segment keys
/// items by provider item id. Both are compared so either form resolves.
export function liveFunctionCallPosition(sessionKey: string, callId: string): number | null {
	const segment = liveSegments.get(sessionKey);
	if (!segment) return null;
	for (const item of segment.items) {
		if (item.payload.type !== "function_call") continue;
		if (item.payload.callId === callId || item.id === callId) return item.position;
	}
	return null;
}

/// Drop the segment tracked for `sessionKey`.
export function clearLiveSegment(sessionKey: string): void {
	liveSegments.delete(sessionKey);
}
