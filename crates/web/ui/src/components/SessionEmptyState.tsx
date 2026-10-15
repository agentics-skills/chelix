import type { VNode } from "preact";
import { toggleSessions } from "../mobile";

export function SessionEmptyState(): VNode {
	return (
		<div className="absolute inset-0 flex items-center justify-center bg-[var(--bg)] text-[var(--muted)]">
			<span className="session-empty-label">No session selected</span>
			<button type="button" className="session-empty-sessions provider-btn" onClick={toggleSessions}>
				<span className="icon icon-chat-dots" />
				Sessions
			</button>
		</div>
	);
}
