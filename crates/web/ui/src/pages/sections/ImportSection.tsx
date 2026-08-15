import type { VNode } from "preact";
import { ChelixDataSection } from "./ChelixDataSection";

export function ImportSection(): VNode {
	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<ChelixDataSection />
		</div>
	);
}
