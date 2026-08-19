// ── Positional placement of live segment nodes ────────────────
//
// Live streaming and reload must produce the same visual order. Reload builds
// it from canonical provider item positions; this module gives the live path
// the same rule, so a node that arrives late but belongs earlier is inserted at
// its own slot instead of being appended to the end of the chat.
//
// Placement never guesses. A node without a resolvable position is a defect in
// whoever produced it, and it is reported instead of being silently appended.

/// Dataset key holding the canonical provider item position of a chat node.
const POSITION_KEY = "providerPosition";

/// Dataset key holding the provider segment a chat node belongs to.
const SEGMENT_KEY = "providerSegment";

export class SegmentPlacementError extends Error {
	constructor(message: string) {
		super(message);
		this.name = "SegmentPlacementError";
	}
}

/// Report a placement defect. Throws so the failure is visible during testing
/// rather than degrading into a wrong but plausible order.
function fail(message: string): never {
	const error = new SegmentPlacementError(message);
	console.error(error.message);
	throw error;
}

/// Mark `node` as belonging to `segmentId` at canonical `position`.
export function markSegmentNode(node: HTMLElement, segmentId: string, position: number): void {
	if (!segmentId) fail("segment node marked without a segment id");
	if (!Number.isSafeInteger(position) || position < 0) {
		fail(`segment node \`${segmentId}\` marked with a non-canonical position: ${String(position)}`);
	}
	node.dataset[SEGMENT_KEY] = segmentId;
	node.dataset[POSITION_KEY] = String(position);
}

/// Canonical position of an already placed node, or `null` when the node does
/// not belong to a provider segment.
function nodePosition(node: HTMLElement): number | null {
	const segmentId = node.dataset[SEGMENT_KEY];
	if (segmentId === undefined) return null;
	const raw = node.dataset[POSITION_KEY];
	if (raw === undefined) fail(`segment node of \`${segmentId}\` has no position`);
	const position = Number(raw);
	if (!Number.isSafeInteger(position)) {
		fail(`segment node of \`${segmentId}\` has an unreadable position: ${raw}`);
	}
	return position;
}

/// First node of `segmentId` that sorts after `position`, or `null` when the
/// new node belongs at the end of the segment.
function successorNode(container: HTMLElement, segmentId: string, position: number): HTMLElement | null {
	for (const child of Array.from(container.children)) {
		if (!(child instanceof HTMLElement)) continue;
		if (child.dataset[SEGMENT_KEY] !== segmentId) continue;
		const childPosition = nodePosition(child);
		if (childPosition === null) continue;
		if (childPosition > position) return child;
	}
	return null;
}

/// Place `node` inside `container` at its canonical slot within `segmentId`.
///
/// Nodes of the same segment stay ordered by provider position regardless of
/// the order their events arrived in. Nodes of other segments are untouched, so
/// segments keep their arrival order relative to each other.
export function placeSegmentNode(container: HTMLElement, node: HTMLElement, segmentId: string, position: number): void {
	markSegmentNode(node, segmentId, position);
	const successor = successorNode(container, segmentId, position);
	if (successor === node) {
		fail(`segment node of \`${segmentId}\` at position ${position} cannot precede itself`);
	}
	// Placement is idempotent. A streamed item is placed again on every chunk,
	// and re-inserting a node that already sits in its slot would detach and
	// re-attach it, forcing a full repaint and resetting the scroll position.
	if (node.parentNode === container && node.nextElementSibling === (successor ?? null)) {
		return;
	}
	if (successor) {
		container.insertBefore(node, successor);
		return;
	}
	container.appendChild(node);
}

/// Whether `node` already carries a canonical placement.
export function isPlaced(node: HTMLElement): boolean {
	return node.dataset[SEGMENT_KEY] !== undefined;
}
