import { sendRpc } from "../helpers";
import type { UiHistoryTarget } from "../types/ui-history";
import { showToast } from "../ui";

interface ForkRequest {
	key: string;
	label?: string;
	target?: UiHistoryTarget;
	forkPoint?: number;
}

export interface ForkResponse {
	sessionKey: string;
	label?: string | null;
	forkPoint: number;
	sourceEnd: number;
	boundaryAdjusted: boolean;
	boundaryReasons: ("active_content" | "interleaved_segment")[];
}

export async function requestSessionFork(request: ForkRequest) {
	const response = await sendRpc<ForkResponse>("sessions.fork", request);
	if (response.ok && response.payload?.boundaryAdjusted) {
		const { forkPoint, sourceEnd, boundaryReasons } = response.payload;
		const reason = boundaryReasons
			.map((value) => (value === "active_content" ? "unfinished content" : "interleaved provider segment"))
			.join(", ");
		showToast(`Forked the confirmed prefix at position ${forkPoint} of ${sourceEnd}: ${reason}.`);
	}
	return response;
}
