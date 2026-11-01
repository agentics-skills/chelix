import { createHighlighterCore } from "shiki/core";
import { createOnigurumaEngine } from "shiki/engine/oniguruma";
import bash from "shiki/langs/bash.mjs";
import githubDark from "shiki/themes/github-dark.mjs";
import githubLight from "shiki/themes/github-light.mjs";
import type { CommandHighlightRequest, CommandHighlightResponse } from "./command-highlight-protocol";

function respond(response: CommandHighlightResponse): void {
	self.postMessage(response);
}

function fail(error: unknown): void {
	self.onmessage = null;
	respond({ type: "error", message: error instanceof Error ? error.message : String(error) });
}

void createHighlighterCore({
	themes: [githubDark, githubLight],
	langs: [bash],
	engine: createOnigurumaEngine(import("shiki/wasm")),
})
	.then((highlighter) => {
		self.onmessage = ({ data }: MessageEvent<CommandHighlightRequest>) => {
			try {
				if (data.type !== "request") throw new Error("Unexpected command highlighting request");
				const html = highlighter.codeToHtml(data.text, {
					lang: "bash",
					themes: { light: "github-light", dark: "github-dark" },
				});
				respond({ type: "result", id: data.id, html });
			} catch (error) {
				fail(error);
			}
		};
		respond({ type: "ready" });
	})
	.catch(fail);
