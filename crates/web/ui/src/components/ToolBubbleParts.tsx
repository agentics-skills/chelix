import type { VNode } from "preact";
import { useLayoutEffect, useRef } from "preact/hooks";
import { bundledLanguagesInfo } from "shiki/langs";
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

const languages = new Map<string, string>();
for (const language of bundledLanguagesInfo) {
	languages.set(language.id, language.id);
	for (const alias of language.aliases ?? []) languages.set(alias, language.id);
}

const filenameLanguages = new Map<string, string>([
	["containerfile", "docker"],
	["gnumakefile", "make"],
	["justfile", "just"],
	["cmakelists.txt", "cmake"],
	[".env", "dotenv"],
	[".bashrc", "shellscript"],
	[".bash_profile", "shellscript"],
	[".zshrc", "shellscript"],
]);

const extensionLanguages = new Map<string, string>([
	["h", "c"],
	["cc", "cpp"],
	["cxx", "cpp"],
	["hpp", "cpp"],
	["hh", "cpp"],
	["hxx", "cpp"],
	["htm", "html"],
	["svg", "xml"],
	["xsd", "xml"],
	["env", "dotenv"],
]);

export function fileLanguage(filePath: unknown): string {
	if (typeof filePath !== "string") return "text";
	const filename = filePath.slice(Math.max(filePath.lastIndexOf("/"), filePath.lastIndexOf("\\")) + 1).toLowerCase();
	const dot = filename.lastIndexOf(".");
	const extension = dot < 0 ? "" : filename.slice(dot + 1);
	return (
		filenameLanguages.get(filename) ??
		languages.get(filename) ??
		extensionLanguages.get(extension) ??
		languages.get(extension) ??
		"text"
	);
}

export function fencedContent(content: string, language: string): string {
	let fenceLength = 3;
	for (const match of content.matchAll(/`+/g)) fenceLength = Math.max(fenceLength, match[0].length + 1);
	const fence = "`".repeat(fenceLength);
	return `${fence}${language}\n${content}\n${fence}`;
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
