import type { VNode } from "preact";
import { render } from "preact";
import { useLayoutEffect, useRef } from "preact/hooks";
import { highlightCodeBlocks } from "../code-highlight";
import { renderMarkdown } from "../helpers";
import { showToast } from "../toast";

interface SubAgentToolBubbleProps {
	parameters: object;
}

interface MetadataField {
	key: string;
	name: string;
	value: unknown;
}

interface MarkdownBodyProps {
	text: string;
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

interface CopyControl {
	button: HTMLButtonElement;
	dispose: () => void;
}

interface ActionPayload {
	name: string;
	parameters: Record<string, unknown>;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

const markdownClasses = [
	"min-w-0 [overflow-wrap:anywhere]",
	"[&_pre]:bg-(--bg)! [&_:not(pre)>code]:bg-(--bg)!",
	"[&_code]:overflow-visible! [&_code]:text-clip!",
	"[&_:not(pre)>code]:inline! [&_:not(pre)>code]:whitespace-pre-wrap!",
	"[&_pre>code]:block! [&_pre>code]:whitespace-pre-wrap!",
	"[&_pre]:my-1! [&_pre]:px-[5px]! [&_pre]:py-1! [&_pre]:overflow-x-auto!",
	"[&_:is(h1,h2,h3,h4,h5,h6)]:mt-1! [&_:is(h1,h2,h3,h4,h5,h6)]:mb-0.5!",
	"[&_:is(ul,ol)]:my-1! [&_ul]:pl-4! [&_ol]:pl-0!",
	"[&_blockquote]:my-1! [&_blockquote]:py-0! [&_blockquote]:px-2!",
	"[&_:where(hr,.msg-table-wrap)]:my-1!",
	"[&>:first-child]:mt-0! [&>:last-child]:mb-0!",
].join(" ");

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

function prepareOrderedLists(body: HTMLElement): void {
	for (const list of body.querySelectorAll<HTMLOListElement>("ol")) {
		list.setAttribute("role", "list");
		list.classList.add("grid", "grid-cols-[max-content_minmax(0,1fr)]", "gap-x-1", "list-none!");
		let number = list.start;
		for (const item of list.querySelectorAll<HTMLLIElement>(":scope > li")) {
			const marker = document.createElement("span");
			marker.className = "whitespace-nowrap text-right";
			marker.textContent = `${number}.`;
			const content = document.createElement("div");
			content.className = "min-w-0";
			while (item.firstChild) content.appendChild(item.firstChild);
			item.classList.add("col-span-2", "grid", "grid-cols-subgrid", "items-baseline", "min-w-0");
			item.append(marker, content);
			number += 1;
		}
	}
}

function createCopyControl(readText: () => string, label: string, feedbackMs: number): CopyControl {
	const button = document.createElement("button");
	button.type = "button";
	button.className = "pointer-events-auto! inline-flex cursor-pointer items-center gap-1 border-0 bg-transparent p-0";
	button.title = "Click to copy";
	button.setAttribute("aria-label", label);
	const icon = document.createElement("span");
	icon.className = "icon icon-copy size-[1em]!";
	icon.setAttribute("aria-hidden", "true");
	button.appendChild(icon);
	const feedbackClasses = ["outline", "outline-1", "outline-(--accent)", "-outline-offset-1"];
	let feedbackTimer: ReturnType<typeof setTimeout> | undefined;
	const onClick = async (event: MouseEvent): Promise<void> => {
		event.stopPropagation();
		try {
			await navigator.clipboard.writeText(readText());
		} catch (error) {
			console.error("[sub_agent] Failed to copy to clipboard:", error);
			showToast("Failed to copy to clipboard", "error");
			return;
		}
		if (!button.isConnected) return;
		clearTimeout(feedbackTimer);
		button.classList.add(...feedbackClasses);
		feedbackTimer = setTimeout(() => button.classList.remove(...feedbackClasses), feedbackMs);
	};
	button.addEventListener("click", onClick);
	return {
		button,
		dispose: () => {
			button.removeEventListener("click", onClick);
			clearTimeout(feedbackTimer);
		},
	};
}

function prepareCodeCopyControls(body: HTMLElement): () => void {
	const controls: CopyControl[] = [];
	for (const pre of body.querySelectorAll<HTMLPreElement>("pre.code-block")) {
		const code = pre.querySelector<HTMLElement>(":scope > code");
		if (!code) throw new Error("sub_agent Markdown code block content is unavailable");
		const control = createCopyControl(
			() => {
				const text = code.textContent;
				if (text === null) throw new Error("sub_agent Markdown code block text is unavailable");
				return text;
			},
			"Copy code",
			250,
		);
		control.button.classList.add("code-lang-badge");
		const badge = pre.querySelector<HTMLElement>(":scope > .code-lang-badge");
		if (badge) {
			control.button.prepend(...badge.childNodes);
			badge.replaceWith(control.button);
		} else {
			pre.appendChild(control.button);
		}
		controls.push(control);
	}
	return () => {
		for (const control of controls) control.dispose();
	};
}

function MarkdownCopyActions({ bodies }: MarkdownCopyActionsProps): VNode {
	const groupRef = useRef<HTMLDivElement>(null);
	useLayoutEffect(() => {
		const group = groupRef.current;
		if (!group) throw new Error("sub_agent Markdown copy controls mount is unavailable");
		const controls = bodies.map((body) => {
			const control = createCopyControl(() => body.text, `Copy ${body.name}`, 300);
			control.button.classList.add("text-[0.7em]", "text-(--muted)", "[font-family:var(--font-mono)]");
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

function MarkdownBody({ text }: MarkdownBodyProps): VNode {
	const bodyRef = useRef<HTMLDivElement>(null);
	useLayoutEffect(() => {
		const body = bodyRef.current;
		if (!body) throw new Error("sub_agent Markdown body is unavailable");
		body.textContent = "";
		body.insertAdjacentHTML("afterbegin", renderMarkdown(text));
		prepareOrderedLists(body);
		const disposeCopyControls = prepareCodeCopyControls(body);
		void highlightCodeBlocks(body);
		return disposeCopyControls;
	}, [text]);
	return <div ref={bodyRef} className={markdownClasses} />;
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
		summary.classList.add("bg-(--surface)!", "[font-family:var(--font-body)]!");
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
