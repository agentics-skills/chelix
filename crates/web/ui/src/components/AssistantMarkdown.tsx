import { render } from "preact";
import { MarkdownBody } from "./MarkdownBody";

const mountedBodies = new Set<HTMLElement>();

export function mountAssistantMarkdown(container: HTMLElement, text: string): void {
	mountedBodies.add(container);
	render(<MarkdownBody text={text} />, container);
}

export function unmountAssistantMarkdown(container: ParentNode): void {
	for (const mount of mountedBodies) {
		if (container === mount || container.contains(mount)) {
			render(null, mount);
			mountedBodies.delete(mount);
		}
	}
}
