// ── Imports section — tabs for each detected import source ────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { TabBar } from "../../components/forms";
import * as gon from "../../gon";
import { sendRpc } from "../../helpers";
import type { RpcResponse } from "./_shared";
import { ChelixDataSection } from "./ChelixDataSection";
import { ClaudeImportSection } from "./ClaudeImportSection";
import { CodexImportSection } from "./CodexImportSection";

interface ImportTabDef {
	id: string;
	label: string;
	icon: VNode;
	detected: boolean;
	detectRpc: string;
	countFn: (payload: Record<string, unknown>) => number;
}

function countClaude(p: Record<string, unknown>): number {
	let n = 0;
	if (p.has_mcp_servers) n++;
	n += (Number(p.skills_count) || 0) + (Number(p.commands_count) || 0);
	if (p.has_memory) n++;
	return n;
}

function countCodex(p: Record<string, unknown>): number {
	let n = Number(p.mcp_servers_count) || 0;
	if (p.has_memory) n++;
	return n;
}

/** Build tab definitions at render time so gon.get() reads current state. */
function getAllTabs(): ImportTabDef[] {
	return [
		{
			id: "claude",
			label: "Claude Code",
			icon: <span className="icon icon-terminal-cmd" />,
			detected: gon.get("claude_detected") === true,
			detectRpc: "claude.detect",
			countFn: countClaude,
		},
		{
			id: "codex",
			label: "Codex CLI",
			icon: <span className="icon icon-code" />,
			detected: gon.get("codex_detected") === true,
			detectRpc: "codex.detect",
			countFn: countCodex,
		},
	];
}

export function ImportSection(): VNode {
	const detectedTabs = getAllTabs().filter((t) => t.detected);
	const [activeTab, setActiveTab] = useState("chelix");
	const [badges, setBadges] = useState<Record<string, number>>({});

	useEffect(() => {
		for (const tab of detectedTabs) {
			sendRpc(tab.detectRpc, {}).then((res: RpcResponse) => {
				if (res?.ok && res.payload) {
					const count = tab.countFn(res.payload as Record<string, unknown>);
					if (count > 0) {
						setBadges((prev) => ({ ...prev, [tab.id]: count }));
					}
				}
			});
		}
	}, []);

	// Chelix tab is always first, then detected external sources.
	const chelixTab = {
		id: "chelix",
		label: "Chelix",
		icon: <span className="icon icon-download" />,
		badge: undefined as number | undefined,
	};

	const externalTabs = detectedTabs.map((t) => ({
		id: t.id,
		label: t.label,
		icon: t.icon,
		badge: badges[t.id] as number | undefined,
	}));

	const tabs = [chelixTab, ...externalTabs];

	// Only Chelix tab — render directly without tab bar
	if (tabs.length === 1) {
		return <div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">{renderTab(tabs[0].id)}</div>;
	}

	return (
		<div className="flex-1 flex flex-col min-w-0 overflow-y-auto">
			<div className="px-4 pt-4">
				<TabBar tabs={tabs} active={activeTab} onChange={setActiveTab} />
			</div>
			<div className="p-4 flex flex-col gap-4">{renderTab(activeTab)}</div>
		</div>
	);
}

function renderTab(id: string): VNode | null {
	switch (id) {
		case "chelix":
			return <ChelixDataSection />;
		case "claude":
			return <ClaudeImportSection />;
		case "codex":
			return <CodexImportSection />;
		default:
			return null;
	}
}
