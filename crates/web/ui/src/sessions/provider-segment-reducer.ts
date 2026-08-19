// ── Provider segment reducer & materializer ────────────────────
//
// Single ordered TypeScript materializer for live streaming and reload.
// Maintains canonical provider items, positions, reasoning parts, and text
// strictly ordered by provider position without DOM-order reliance.

import type {
	ProviderItemUpdate,
	ProviderItemUpdatePayload,
	ProviderOutputItem,
	ProviderOutputPayload,
	ProviderSegmentOutcome,
	ReasoningContent,
	ReasoningPart,
} from "../types/ws-events";

export interface ProviderSegmentViewModel {
	segmentId: string;
	outcome: ProviderSegmentOutcome;
	items: ProviderOutputItem[];
	lastUpdateSeq: Map<string, number>;
}

export function createProviderSegmentViewModel(segmentId: string): ProviderSegmentViewModel {
	return {
		segmentId,
		outcome: "active",
		items: [],
		lastUpdateSeq: new Map(),
	};
}

export function applyProviderItemUpdate(segment: ProviderSegmentViewModel, update: ProviderItemUpdate): void {
	if (segment.outcome !== "active") return;
	const lastSeq = segment.lastUpdateSeq.get(update.itemId) ?? 0;
	if (update.updateSeq <= lastSeq) return;
	segment.lastUpdateSeq.set(update.itemId, update.updateSeq);

	let existing = segment.items.find((item) => item.id === update.itemId);
	if (existing) {
		applyPayloadToItem(existing, update);
	} else {
		const payload = createPayloadFromUpdate(update);
		existing = {
			id: update.itemId,
			position: update.position,
			payload,
		};
		segment.items.push(existing);
		segment.items.sort((a, b) => a.position - b.position);
	}
}

function createPayloadFromUpdate(update: ProviderItemUpdate): ProviderOutputItem["payload"] {
	const p = update.payload;
	switch (p.update_type) {
		case "reasoning_delta":
			return {
				type: "reasoning",
				id: update.itemId,
				outputIndex: update.position,
				summaryParts: [{ partIndex: p.part_index, text: p.delta }],
			};
		case "reasoning_part_done":
			return {
				type: "reasoning",
				id: update.itemId,
				outputIndex: update.position,
				summaryParts: [{ partIndex: p.part_index, text: p.text }],
			};
		case "reasoning_item_done":
			return {
				type: "reasoning",
				id: update.itemId,
				outputIndex: update.position,
				summaryParts: [],
			};
		case "reasoning_text":
			return {
				type: "reasoning",
				id: update.itemId,
				outputIndex: update.position,
				visibleText: p.text,
			};
		case "reasoning_text_delta":
			return {
				type: "reasoning",
				id: update.itemId,
				outputIndex: update.position,
				visibleText: p.delta,
			};
		case "message_delta":
			return {
				type: "message",
				text: p.delta,
			};
		case "message_done":
			return {
				type: "message",
				text: p.text,
			};
		case "function_call_start":
			return {
				type: "function_call",
				callId: update.itemId,
				name: p.name,
				arguments: "",
			};
		case "function_call_delta":
			return {
				type: "function_call",
				callId: update.itemId,
				name: "",
				arguments: p.delta,
			};
		case "function_call_done":
			return {
				type: "function_call",
				callId: update.itemId,
				name: "",
				arguments: p.arguments,
			};
	}
}

type ReasoningPayload = Extract<ProviderOutputPayload, { type: "reasoning" }>;
type MessagePayload = Extract<ProviderOutputPayload, { type: "message" }>;
type FunctionCallPayload = Extract<ProviderOutputPayload, { type: "function_call" }>;

function upsertSummaryPart(target: ReasoningPayload, partIndex: number, text: string, append: boolean): void {
	const summaryParts: ReasoningPart[] = target.summaryParts ?? [];
	target.summaryParts = summaryParts;
	const existing = summaryParts.find((candidate) => candidate.partIndex === partIndex);
	if (existing) {
		existing.text = append ? existing.text + text : text;
		return;
	}
	summaryParts.push({ partIndex, text });
	summaryParts.sort((a, b) => a.partIndex - b.partIndex);
}

function applyReasoningUpdate(target: ReasoningPayload, payload: ProviderItemUpdatePayload): void {
	switch (payload.update_type) {
		case "reasoning_delta":
			upsertSummaryPart(target, payload.part_index, payload.delta, true);
			return;
		case "reasoning_part_done":
			upsertSummaryPart(target, payload.part_index, payload.text, false);
			return;
		case "reasoning_text":
			target.visibleText = payload.text;
			return;
		case "reasoning_text_delta":
			target.visibleText = (target.visibleText ?? "") + payload.delta;
			return;
		default:
			return;
	}
}

function applyMessageUpdate(target: MessagePayload, payload: ProviderItemUpdatePayload): void {
	if (payload.update_type === "message_delta") {
		target.text += payload.delta;
		return;
	}
	if (payload.update_type === "message_done") {
		target.text = payload.text;
	}
}

function applyFunctionCallUpdate(target: FunctionCallPayload, payload: ProviderItemUpdatePayload): void {
	switch (payload.update_type) {
		case "function_call_start":
			target.name = payload.name;
			return;
		case "function_call_delta":
			target.arguments += payload.delta;
			return;
		case "function_call_done":
			target.arguments = payload.arguments;
			return;
		default:
			return;
	}
}

function applyPayloadToItem(item: ProviderOutputItem, update: ProviderItemUpdate): void {
	const current = item.payload;
	if (current.type === "reasoning") {
		applyReasoningUpdate(current, update.payload);
		return;
	}
	if (current.type === "message") {
		applyMessageUpdate(current, update.payload);
		return;
	}
	applyFunctionCallUpdate(current, update.payload);
}

export function extractSegmentReasoning(segment: ProviderSegmentViewModel): ReasoningContent {
	const parts: string[] = [];
	let visibleText = "";
	for (const item of segment.items) {
		if (item.payload.type !== "reasoning") continue;
		const summaryParts = item.payload.summaryParts ?? [];
		if (summaryParts.length > 0) {
			for (const part of summaryParts) {
				if (part.text) parts.push(part.text);
			}
			continue;
		}
		visibleText += item.payload.visibleText ?? "";
	}
	return parts.length > 0 ? parts : visibleText;
}

export function extractSegmentMessageText(segment: ProviderSegmentViewModel): string {
	let text = "";
	for (const item of segment.items) {
		if (item.payload.type === "message") {
			text += item.payload.text;
		}
	}
	return text;
}

/// Reasoning display content for one canonical provider item.
export function extractItemReasoning(item: ProviderOutputItem): ReasoningContent {
	if (item.payload.type !== "reasoning") return "";
	const summaryParts = item.payload.summaryParts ?? [];
	if (summaryParts.length > 0) return summaryParts.map((part) => part.text);
	return item.payload.visibleText ?? "";
}

/// Build a segment view model from already materialized provider items.
export function segmentFromItems(segmentId: string, items: ProviderOutputItem[]): ProviderSegmentViewModel {
	const segment = createProviderSegmentViewModel(segmentId);
	segment.items = [...items].sort((a, b) => a.position - b.position);
	return segment;
}
