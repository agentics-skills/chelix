import type { VNode } from "preact";
import { render } from "preact";
import { MarkdownBody } from "./MarkdownBody";
import { type InputBody, MarkdownCopyActions, type MetadataField, ToolBubbleMetadata } from "./ToolBubbleParts";

interface SubAgentToolBubbleProps {
	parameters: object;
}

interface SubAgentInput {
	metadata: MetadataField[];
	bodies: InputBody[];
}

interface ActionPayload {
	name: string;
	parameters: Record<string, unknown>;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

function isParameterObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function actionPayload(value: unknown): ActionPayload | null {
	if (!isParameterObject(value)) return null;
	const entries = Object.entries(value);
	if (entries.length !== 1) return null;
	const [name, parameters] = entries[0];
	return isParameterObject(parameters) ? { name, parameters } : null;
}

function splitInput(parameters: object): SubAgentInput {
	const metadata: MetadataField[] = [];
	const bodies: InputBody[] = [];
	const entries: [string, unknown][] = Object.entries(parameters);
	for (const [name, value] of entries) {
		const action = name === "action" ? actionPayload(value) : null;
		if (!action) {
			metadata.push({ key: `root:${name}`, name, value });
			continue;
		}
		metadata.push({ key: "root:action", name: "action", value: action.name });
		for (const [fieldName, fieldValue] of Object.entries(action.parameters)) {
			if ((fieldName === "message" || fieldName === "task") && typeof fieldValue === "string") {
				bodies.push({ name: fieldName, text: fieldValue });
			} else {
				metadata.push({ key: `payload:${fieldName}`, name: fieldName, value: fieldValue });
			}
		}
	}
	return { metadata, bodies: bodies.filter((body) => body.text.trim()) };
}

function SubAgentToolBubble({ parameters }: SubAgentToolBubbleProps): VNode {
	const { metadata, bodies } = splitInput(parameters);
	return (
		<div className="relative flex w-full min-w-0 flex-col gap-0.5">
			<ToolBubbleMetadata fields={metadata} />
			<MarkdownCopyActions bodies={bodies} />
			{bodies.map((body) => (
				<MarkdownBody key={body.name} text={body.text} />
			))}
		</div>
	);
}

export function mountSubAgentToolBubble(card: HTMLElement, options: SubAgentToolBubbleProps): void {
	let mount = mountedBubbles.get(card);
	if (!mount) {
		const summary = card.querySelector<HTMLElement>(".tool-call-header > .tool-call-summary");
		if (!summary) throw new Error("sub_agent tool card summary mount is unavailable");
		summary.textContent = "";
		summary.setAttribute("data-sub-agent-bubble", "");
		mount = summary;
		mountedBubbles.set(card, mount);
	}
	render(<SubAgentToolBubble {...options} />, mount);
}

export function unmountSubAgentToolBubble(card: HTMLElement): void {
	const mount = mountedBubbles.get(card);
	if (!mount) return;
	render(null, mount);
	mountedBubbles.delete(card);
}

export function unmountSubAgentToolBubbles(container: ParentNode): void {
	for (const [card] of mountedBubbles) {
		if (container === card || container.contains(card)) unmountSubAgentToolBubble(card);
	}
}
