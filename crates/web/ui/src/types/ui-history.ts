import type { HistoryMessage } from "./session";

export interface UiHistoryTarget {
	messageId: string;
	generation: string;
}

export interface UiSnapshot extends HistoryMessage {
	id: string;
	position: number;
	revision: number;
	canonicalCommitted: boolean;
	clientMessageId?: string;
	assistantId?: string;
	generation?: string;
	presentation: {
		document?: { format: "text" | "markdown" | "diff"; content: string };
		metadata: Record<string, unknown>;
	};
}

export type UiHistoryRange =
	| { direction: "latest" }
	| { direction: "around"; message_id: string }
	| { direction: "window"; start: number; end: number | null };

export interface UiHistoryPage {
	generation: string;
	revision: number;
	totalMessages: number;
	history: UiSnapshot[];
	hasOlder: boolean;
	hasNewer: boolean;
	firstPosition: number | null;
	lastPosition: number | null;
}

export interface UiHistoryBatch {
	generation: string;
	fromRevision: number;
	revision: number;
	totalMessages: number;
	history: UiSnapshot[];
}

export interface UiHistoryEvent {
	subscriptionId: string;
	sessionKey: string;
	snapshot?: UiHistoryPage;
	update?: UiHistoryBatch;
}
