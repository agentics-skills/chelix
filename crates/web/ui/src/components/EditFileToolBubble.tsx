import type { VNode } from "preact";
import { render } from "preact";
import { MarkdownBody } from "./MarkdownBody";
import { type InputBody, MarkdownCopyActions, type MetadataField, ToolBubbleMetadata } from "./ToolBubbleParts";

interface EditFileToolBubbleProps {
	toolName: "edit_file" | "multiedit_file";
	parameters: object;
}

interface EditBody extends InputBody {
	metadata: MetadataField[];
}

interface EditFileInput {
	metadata: MetadataField[];
	edits: EditBody[];
}

const mountedBubbles = new Map<HTMLElement, HTMLElement>();

function isParameterObject(value: unknown): value is Record<string, unknown> {
	return value !== null && typeof value === "object" && !Array.isArray(value);
}

function diffLines(text: string, prefix: "-" | "+"): string[] {
	if (text.length === 0) return [];
	return text
		.replace(/\r\n?/g, "\n")
		.split("\n")
		.map((line) => `${prefix}${line}`);
}

function appendEdit(input: EditFileInput, name: string, value: unknown): void {
	if (!isParameterObject(value) || typeof value.oldString !== "string" || typeof value.newString !== "string") {
		input.metadata.push({ key: name, name, value });
		return;
	}
	const metadata = Object.entries(value)
		.filter(([field]) => field !== "oldString" && field !== "newString")
		.map(([field, fieldValue]) => ({ key: `${name}:${field}`, name: field, value: fieldValue }));
	const text = ["```diff", ...diffLines(value.oldString, "-"), ...diffLines(value.newString, "+"), "```"].join("\n");
	input.edits.push({ name, text, metadata });
}

function splitInput({ toolName, parameters }: EditFileToolBubbleProps): EditFileInput {
	const input: EditFileInput = { metadata: [], edits: [] };
	const operationField = toolName === "edit_file" ? "edit" : "edits";
	const entries: [string, unknown][] = Object.entries(parameters);
	for (const [name, value] of entries) {
		if (name === operationField && name === "edit") {
			appendEdit(input, name, value);
		} else if (name === operationField && Array.isArray(value) && value.length > 0) {
			value.forEach((edit: unknown, index) => {
				appendEdit(input, `${name}[${index}]`, edit);
			});
		} else {
			input.metadata.push({ key: `root:${name}`, name, value });
		}
	}
	return input;
}

function EditFileToolBubble(options: EditFileToolBubbleProps): VNode {
	const { metadata, edits } = splitInput(options);
	const bodies: InputBody[] = edits.length
		? [{ name: options.toolName === "edit_file" ? "edit" : "edits", text: edits.map((edit) => edit.text).join("\n\n") }]
		: [];
	return (
		<div className="relative flex w-full min-w-0 flex-col gap-0.5">
			<ToolBubbleMetadata fields={metadata} />
			<MarkdownCopyActions bodies={bodies} />
			{edits.map((edit) => (
				<div key={edit.name} className="min-w-0">
					<div className="flex min-w-0 flex-wrap items-baseline gap-x-1 text-[0.62rem] leading-[1.2]">
						<span>{edit.name}</span>
						<ToolBubbleMetadata fields={edit.metadata} />
					</div>
					<MarkdownBody text={edit.text} />
				</div>
			))}
		</div>
	);
}

export function mountEditFileToolBubble(card: HTMLElement, options: EditFileToolBubbleProps): void {
	let mount = mountedBubbles.get(card);
	if (!mount) {
		const summary = card.querySelector<HTMLElement>(".tool-call-header > .tool-call-summary");
		if (!summary) throw new Error(`${options.toolName} tool card summary mount is unavailable`);
		summary.textContent = "";
		summary.setAttribute("data-edit-file-bubble", "");
		mount = summary;
		mountedBubbles.set(card, mount);
	}
	render(<EditFileToolBubble {...options} />, mount);
}

export function unmountEditFileToolBubble(card: HTMLElement): void {
	const mount = mountedBubbles.get(card);
	if (!mount) return;
	render(null, mount);
	mountedBubbles.delete(card);
}

export function unmountEditFileToolBubbles(container: ParentNode): void {
	for (const [card] of mountedBubbles) {
		if (container === card || container.contains(card)) unmountEditFileToolBubble(card);
	}
}
