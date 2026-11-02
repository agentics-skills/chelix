import type { VNode } from "preact";
import { render } from "preact";
import { MarkdownBody } from "./MarkdownBody";
import {
	fencedContent,
	type InputBody,
	MarkdownCopyActions,
	type MetadataField,
	ToolBubbleMetadata,
} from "./ToolBubbleParts";

interface ExecuteCommandInputBubbleProps {
	parameters: object;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

function ExecuteCommandInputBubble({ parameters }: ExecuteCommandInputBubbleProps): VNode {
	const entries: [string, unknown][] = Object.entries(parameters);
	const metadata: MetadataField[] = entries
		.filter(([name, value]) => name !== "command" || typeof value !== "string")
		.map(([name, value]) => ({ key: `root:${name}`, name, value }));
	const command = "command" in parameters ? parameters.command : undefined;
	const bodies: InputBody[] = typeof command === "string" ? [{ name: "command", text: command }] : [];
	return (
		<div className="relative flex w-full min-w-0 flex-col gap-0.5">
			<ToolBubbleMetadata fields={metadata} />
			<MarkdownCopyActions bodies={bodies} />
			{bodies.map((body) => (
				<MarkdownBody key={body.name} text={fencedContent(body.text, "shell")} asciiTables={false} />
			))}
		</div>
	);
}

export function mountExecuteCommandInputBubble(card: HTMLElement, options: ExecuteCommandInputBubbleProps): void {
	let mount = mountedBubbles.get(card);
	if (!mount) {
		const summary = card.querySelector<HTMLElement>(".tool-call-header > .tool-call-summary");
		if (!summary) throw new Error("execute_command tool card summary mount is unavailable");
		summary.textContent = "";
		summary.setAttribute("data-execute-command-input-bubble", "");
		mount = summary;
		mountedBubbles.set(card, mount);
	}
	render(<ExecuteCommandInputBubble {...options} />, mount);
}

export function unmountExecuteCommandInputBubble(card: HTMLElement): void {
	const mount = mountedBubbles.get(card);
	if (!mount) return;
	render(null, mount);
	mountedBubbles.delete(card);
}

export function unmountExecuteCommandInputBubbles(container: ParentNode): void {
	for (const [card] of mountedBubbles) {
		if (container === card || container.contains(card)) unmountExecuteCommandInputBubble(card);
	}
}
