import type { VNode } from "preact";
import { useLayoutEffect, useRef } from "preact/hooks";
import { highlightCodeBlocks } from "../code-highlight";
import { renderMarkdown } from "../helpers";
import { showToast } from "../toast";

export interface MarkdownBodyProps {
	text: string;
	asciiTables?: boolean;
}

interface CopyControl {
	button: HTMLButtonElement;
	dispose: () => void;
}

const markdownClasses = [
	"min-w-0 [overflow-wrap:anywhere]",
	"[&_pre]:bg-(--bg)! [&_:not(pre)>code]:bg-(--bg)!",
	"[&_pre]:text-[0.8rem]! [&_pre]:leading-[1.45]!",
	"[&_:not(pre)>code]:text-[0.68rem]!",
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

export function createCopyControl(readText: () => string, label: string, feedbackMs: number): CopyControl {
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
			console.error("[markdown] Failed to copy to clipboard:", error);
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
		if (!code) throw new Error("Markdown code block content is unavailable");
		const control = createCopyControl(
			() => {
				const text = code.textContent;
				if (text === null) throw new Error("Markdown code block text is unavailable");
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

function reuseHighlightedCode(body: HTMLElement, previous: Map<string, HTMLElement[]>): Map<string, HTMLElement[]> {
	const current = new Map<string, HTMLElement[]>();
	for (const code of body.querySelectorAll<HTMLElement>("pre > code[data-lang]")) {
		const key = JSON.stringify([code.getAttribute("data-lang"), code.textContent]);
		const retained = previous.get(key)?.pop();
		let node = code;
		if (retained?.classList.contains("shiki")) {
			code.replaceWith(retained);
			node = retained;
		}
		const nodes = current.get(key);
		if (nodes) nodes.push(node);
		else current.set(key, [node]);
	}
	return current;
}

export function MarkdownBody({ text, asciiTables = true }: MarkdownBodyProps): VNode {
	const bodyRef = useRef<HTMLDivElement>(null);
	const codeBlocksRef = useRef<Map<string, HTMLElement[]>>(new Map());
	useLayoutEffect(() => {
		const body = bodyRef.current;
		if (!body) throw new Error("Markdown body is unavailable");
		body.textContent = "";
		body.insertAdjacentHTML("afterbegin", renderMarkdown(text, asciiTables));
		codeBlocksRef.current = reuseHighlightedCode(body, codeBlocksRef.current);
		prepareOrderedLists(body);
		const disposeCopyControls = prepareCodeCopyControls(body);
		const controller = new AbortController();
		void highlightCodeBlocks(body, controller.signal);
		return () => {
			controller.abort();
			disposeCopyControls();
		};
	}, [text, asciiTables]);
	return <div ref={bodyRef} className={markdownClasses} />;
}
