import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect } from "preact/hooks";
import { TerminalAttachment } from "./TerminalAttachment";

const TERMINAL_ATTACH_DELAY_MS = 10_000;

interface ExecuteCommandToolBubbleProps {
	sessionKey: string;
	toolCallId: string;
	startedAt: number;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

function validStartedAt(startedAt: number): boolean {
	return Number.isSafeInteger(startedAt) && startedAt > 0;
}

function ExecuteCommandToolBubble({ sessionKey, toolCallId, startedAt }: ExecuteCommandToolBubbleProps): VNode {
	const deadlineReached = useSignal(validStartedAt(startedAt) && Date.now() >= startedAt + TERMINAL_ATTACH_DELAY_MS);
	const status = useSignal("");
	const statusLevel = useSignal<"" | "ok" | "error">("");

	useEffect(() => {
		if (!validStartedAt(startedAt)) {
			status.value = "Tool call start timestamp is unavailable or invalid.";
			statusLevel.value = "error";
			return;
		}
		const remaining = startedAt + TERMINAL_ATTACH_DELAY_MS - Date.now();
		if (remaining <= 0) {
			deadlineReached.value = true;
			return;
		}
		const wakeUp = window.setTimeout(() => {
			deadlineReached.value = true;
		}, remaining);
		return () => window.clearTimeout(wakeUp);
	}, [startedAt]);

	if (!validStartedAt(startedAt)) {
		return (
			<div className="tool-call-result-placeholder text-[var(--err)]" role="alert">
				{status.value}
			</div>
		);
	}
	if (!deadlineReached.value) {
		return <div className="tool-call-result-placeholder">Waiting for tool result…</div>;
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

export function mountExecuteCommandToolBubble(
	card: HTMLElement,
	options: ExecuteCommandToolBubbleProps,
): void {
	if (mountedBubbles.has(card)) return;
	const content = card.querySelector<HTMLElement>("[data-tool-result-content]");
	if (!content) throw new Error("execute_command tool card result mount is unavailable");
	content.textContent = "";
	const mount = document.createElement("div");
	mount.setAttribute("data-execute-command-bubble", "");
	content.appendChild(mount);
	mountedBubbles.set(card, mount);
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
