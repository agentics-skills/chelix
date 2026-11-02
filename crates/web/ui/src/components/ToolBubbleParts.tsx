import type { VNode } from "preact";
import { useLayoutEffect, useRef } from "preact/hooks";
import { createCopyControl, type MarkdownBodyProps } from "./MarkdownBody";

export interface MetadataField {
	key: string;
	name: string;
	value: unknown;
}

export interface InputBody extends MarkdownBodyProps {
	name: string;
}

interface ToolBubbleMetadataProps {
	fields: MetadataField[];
}

interface MarkdownCopyActionsProps {
	bodies: InputBody[];
}

export function ToolBubbleMetadata({ fields }: ToolBubbleMetadataProps): VNode {
	return (
		<dl className="m-0 flex min-w-0 flex-wrap gap-x-1 text-[0.62rem] leading-[1.2]">
			{fields.map((field) => (
				<div key={field.key} className="min-w-0 [overflow-wrap:anywhere]">
					<dt className="inline">{field.name}: </dt>
					<dd className="m-0 inline">{typeof field.value === "string" ? field.value : JSON.stringify(field.value)}</dd>
				</div>
			))}
		</dl>
	);
}

export function MarkdownCopyActions({ bodies }: MarkdownCopyActionsProps): VNode {
	const groupRef = useRef<HTMLDivElement>(null);
	useLayoutEffect(() => {
		const group = groupRef.current;
		if (!group) throw new Error("Tool Markdown copy controls mount is unavailable");
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
