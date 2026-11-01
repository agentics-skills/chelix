export interface CommandHighlightRequest {
	type: "request";
	id: number;
	text: string;
}

export type CommandHighlightResponse =
	| { type: "ready" }
	| { type: "result"; id: number; html: string }
	| { type: "error"; message: string };
