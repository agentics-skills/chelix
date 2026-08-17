import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { TerminalAttachment } from "./TerminalAttachment";

interface ExecuteCommandToolBubbleProps {
	sessionKey: string;
	toolCallId: string;
	progressMessage: string;
	attachTerminal: boolean;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

function ExecuteCommandToolBubble({
	sessionKey,
	toolCallId,
	progressMessage,
	attachTerminal,
}: ExecuteCommandToolBubbleProps): VNode {
	const status = useSignal("");
	const statusLevel = useSignal<"" | "ok" | "error">("");

	if (!attachTerminal) {
		return <div className="tool-call-result-placeholder">{progressMessage || "Waiting for tool result…"}</div>;
	}
	return (
		<div className="overflow-hidden rounded border border-[var(--border)] bg-[var(--bg)]">
			<TerminalAttachment
				connection={{ mode: "tool_call", sessionKey, toolCallId }}
				className="terminal-output chat-terminal-output h-24 min-h-0"
				ariaLabel="Execute command terminal output"
				focusOnReady={false}
				onStatus={(text, level) => {
					status.value = text;
					statusLevel.value = level;
				}}
			/>
			{statusLevel.value === "error" ? (
				<div className="px-2 py-1 text-xs text-[var(--err)]" role="alert">
					{status.value}
				</div>
			) : null}
		</div>
	);
}

export function mountExecuteCommandToolBubble(card: HTMLElement, options: ExecuteCommandToolBubbleProps): void {
	let mount = mountedBubbles.get(card);
	if (!mount) {
		const content = card.querySelector<HTMLElement>("[data-tool-result-content]");
		if (!content) throw new Error("execute_command tool card result mount is unavailable");
		content.textContent = "";
		mount = document.createElement("div");
		mount.setAttribute("data-execute-command-bubble", "");
		content.appendChild(mount);
		mountedBubbles.set(card, mount);
	}
	render(<ExecuteCommandToolBubble {...options} />, mount);
}

export function unmountExecuteCommandToolBubble(card: HTMLElement): void {
	const mount = mountedBubbles.get(card);
	if (!mount) return;
	render(null, mount);
	mount.remove();
	mountedBubbles.delete(card);
}

export function unmountExecuteCommandToolBubbles(container: ParentNode): void {
	for (const [card] of mountedBubbles) {
		if (container === card || container.contains(card)) unmountExecuteCommandToolBubble(card);
	}
}
