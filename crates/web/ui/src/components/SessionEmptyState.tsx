import type { VNode } from "preact";

export function SessionEmptyState(): VNode {
	return (
		<div className="absolute inset-0 flex items-center justify-center bg-[var(--bg)] text-[var(--muted)]">
			No session selected
		</div>
	);
}
