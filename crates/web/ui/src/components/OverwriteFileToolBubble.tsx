import type { VNode } from "preact";
import { render } from "preact";
import { bundledLanguagesInfo } from "shiki/langs";
import { MarkdownBody } from "./MarkdownBody";
import {
	fencedContent,
	type InputBody,
	MarkdownCopyActions,
	type MetadataField,
	ToolBubbleMetadata,
} from "./ToolBubbleParts";

interface OverwriteFileToolBubbleProps {
	parameters: object;
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();
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

function fileLanguage(filePath: unknown): string {
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
