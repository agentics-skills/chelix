import type { VNode } from "preact";
import { render } from "preact";
import { useLayoutEffect, useRef } from "preact/hooks";
import { createCopyControl, MarkdownBody, type MarkdownBodyProps } from "./MarkdownBody";

interface SubAgentToolBubbleProps {
	parameters: object;
}

interface MetadataField {
	key: string;
	name: string;
	value: unknown;
}

interface InputBody extends MarkdownBodyProps {
	name: string;
}

interface SubAgentInput {
	metadata: MetadataField[];
	bodies: InputBody[];
}

interface MarkdownCopyActionsProps {
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

function MarkdownCopyActions({ bodies }: MarkdownCopyActionsProps): VNode {
	const groupRef = useRef<HTMLDivElement>(null);
	useLayoutEffect(() => {
		const group = groupRef.current;
		if (!group) throw new Error("sub_agent Markdown copy controls mount is unavailable");
		const controls = bodies.map((body) => {
			const control = createCopyControl(() => body.text, `Copy ${body.name}`, 300);
			control.button.classList.add("text-[0.56rem]", "text-(--muted)", "[font-family:var(--font-mono)]");
			group.appendChild(control.button);
			return control;
		});
		return () => {
			for (const control of controls) {
				control.dispose();
				control.button.remove();
			}
		};
	}, [bodies]);
	return <div ref={groupRef} className="absolute top-0 right-0 z-10 flex" />;
}

function SubAgentToolBubble({ parameters }: SubAgentToolBubbleProps): VNode {
	const { metadata, bodies } = splitInput(parameters);
	return (
		<div className="relative flex w-full min-w-0 flex-col gap-0.5">
			<dl className="m-0 flex min-w-0 flex-wrap gap-x-1 text-[0.62rem] leading-[1.2]">
				{metadata.map((field) => (
					<div key={field.key} className="min-w-0 [overflow-wrap:anywhere]">
						<dt className="inline">{field.name}: </dt>
						<dd className="m-0 inline">
							{typeof field.value === "string" ? field.value : JSON.stringify(field.value)}
						</dd>
					</div>
				))}
			</dl>
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
