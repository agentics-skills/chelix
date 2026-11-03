import type { VNode } from "preact";
import { render } from "preact";
import { MarkdownBody } from "./MarkdownBody";
import {
	fencedContent,
	fileLanguage,
	type InputBody,
	MarkdownCopyActions,
	type MetadataField,
	ToolBubbleMetadata,
} from "./ToolBubbleParts";

interface OverwriteFileToolBubbleProps {
	parameters: object;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();
function OverwriteFileToolBubble({ parameters }: OverwriteFileToolBubbleProps): VNode {
	const entries: [string, unknown][] = Object.entries(parameters);
	const metadata: MetadataField[] = entries
		.filter(([name, value]) => name !== "content" || typeof value !== "string")
		.map(([name, value]) => ({ key: `root:${name}`, name, value }));
	const content = "content" in parameters ? parameters.content : undefined;
	const filePath = "filePath" in parameters ? parameters.filePath : undefined;
	const bodies: InputBody[] = typeof content === "string" ? [{ name: "content", text: content }] : [];
	const language = fileLanguage(filePath);
	return (
		<div className="relative flex w-full min-w-0 flex-col gap-0.5">
			<ToolBubbleMetadata fields={metadata} />
			<MarkdownCopyActions bodies={bodies} />
			{bodies.map((body) => (
				<MarkdownBody key={body.name} text={fencedContent(body.text, language)} asciiTables={false} />
			))}
		</div>
	);
}

export function mountOverwriteFileToolBubble(card: HTMLElement, options: OverwriteFileToolBubbleProps): void {
	let mount = mountedBubbles.get(card);
	if (!mount) {
		const summary = card.querySelector<HTMLElement>(".tool-call-header > .tool-call-summary");
		if (!summary) throw new Error("overwrite_file tool card summary mount is unavailable");
		summary.textContent = "";
		summary.setAttribute("data-overwrite-file-bubble", "");
		mount = summary;
		mountedBubbles.set(card, mount);
	}
	render(<OverwriteFileToolBubble {...options} />, mount);
}

export function unmountOverwriteFileToolBubble(card: HTMLElement): void {
	const mount = mountedBubbles.get(card);
	if (!mount) return;
	render(null, mount);
	mountedBubbles.delete(card);
}

export function unmountOverwriteFileToolBubbles(container: ParentNode): void {
	for (const [card] of mountedBubbles) {
		if (container === card || container.contains(card)) unmountOverwriteFileToolBubble(card);
	}
}
