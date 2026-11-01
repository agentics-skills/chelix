// Shiki decoration for Markdown blocks and retained command previews.

import type { BundledLanguage, BundledTheme, HighlighterGeneric } from "shiki";
import type { CommandHighlightRequest, CommandHighlightResponse } from "./command-highlight-protocol";
import { showToast } from "./toast";

type Highlighter = HighlighterGeneric<BundledLanguage, BundledTheme>;

let highlighter: Highlighter | null = null;
let highlighterInitPromise: Promise<Highlighter> | null = null;
let bundledLanguages: Set<string> | null = null;
const languageLoadPromises = new Map<string, Promise<void>>();
const failedCodeElements = new WeakSet<HTMLElement>();
const reportedErrors = new WeakSet<Error>();

function highlightError(error: unknown): Error {
	return error instanceof Error ? error : new Error(String(error));
}

export function reportHighlightError(error: unknown): void {
	const failure = highlightError(error);
	if (reportedErrors.has(failure)) return;
	reportedErrors.add(failure);
	console.error("[shiki] syntax highlighting failed:", failure);
	showToast(`Syntax highlighting failed: ${failure.message}`, "error");
}

/** Initialize the main-thread highlighter and retain its initialization result. */
export function initHighlighter(): Promise<Highlighter> {
	if (!highlighterInitPromise) {
		highlighterInitPromise = (async () => {
			try {
				const shiki = await import("shiki");
				const instance = await shiki.createHighlighter({
					themes: ["github-dark", "github-light"],
					langs: [],
				});
				bundledLanguages = new Set(Object.keys(shiki.bundledLanguages));
				highlighter = instance;
				return instance;
			} catch (error) {
				throw highlightError(error);
			}
		})();
	}
	return highlighterInitPromise;
}

/** Returns whether the main-thread highlighter has finished loading. */
export function isReady(): boolean {
	return highlighter !== null;
}

async function ensureLanguageLoaded(lang: string): Promise<boolean> {
	const instance = highlighter;
	if (!(instance && bundledLanguages)) throw new Error("Syntax highlighter is not initialized");
	if (!bundledLanguages.has(lang)) return false;
	if (instance.getLoadedLanguages().includes(lang)) return true;
	let inFlight = languageLoadPromises.get(lang);
	if (!inFlight) {
		inFlight = (async () => {
			try {
				await instance.loadLanguage(lang as BundledLanguage);
				if (!instance.getLoadedLanguages().includes(lang)) {
					throw new Error(`Syntax grammar was not loaded: ${lang}`);
				}
			} catch (error) {
				throw highlightError(error);
			}
		})();
		languageLoadPromises.set(lang, inFlight);
	}
	await inFlight;
	return true;
}

function applyShikiMarkupToCode(codeEl: HTMLElement, shikiPre: HTMLElement): void {
	const shikiCode = shikiPre.querySelector("code");
	if (!shikiCode) throw new Error("Syntax highlighting returned markup without code");
	codeEl.textContent = "";
	while (shikiCode.firstChild) codeEl.appendChild(shikiCode.firstChild);
	codeEl.classList.add("shiki");
	for (const cls of shikiPre.classList) {
		if (cls !== "shiki") codeEl.classList.add(cls);
	}
}

function parseShikiPre(highlightedHtml: string): HTMLElement {
	const temp = document.createElement("div");
	// Shiki escapes source text and emits syntax-highlighted markup.
	const range = document.createRange();
	temp.appendChild(range.createContextualFragment(highlightedHtml));
	const pre = temp.querySelector<HTMLElement>("pre.shiki");
	if (!pre) throw new Error("Syntax highlighting returned markup without pre.shiki");
	return pre;
}

function highlightedMarkup(text: string, lang: string): HTMLElement {
	if (!highlighter) throw new Error("Syntax highlighter is not initialized");
	return parseShikiPre(
		highlighter.codeToHtml(text, {
			lang,
			themes: { light: "github-light", dark: "github-dark" },
		}),
	);
}

async function highlightCodeElement(codeEl: HTMLElement): Promise<void> {
	if (codeEl.querySelector(".shiki") || codeEl.classList.contains("shiki")) return;
	const lang = codeEl.getAttribute("data-lang") || "";
	if (!(await ensureLanguageLoaded(lang))) return;
	applyShikiMarkupToCode(codeEl, highlightedMarkup(codeEl.textContent || "", lang));
}

export async function highlightCodeBlocks(containerEl: HTMLElement | null): Promise<void> {
	if (!containerEl) return;
	try {
		await initHighlighter();
	} catch (error) {
		reportHighlightError(error);
		return;
	}
	for (const codeEl of containerEl.querySelectorAll<HTMLElement>("pre code[data-lang]")) {
		if (failedCodeElements.has(codeEl)) continue;
		try {
			await highlightCodeElement(codeEl);
		} catch (error) {
			failedCodeElements.add(codeEl);
			reportHighlightError(error);
		}
	}
}

interface CommandState {
	container: HTMLElement;
	code: HTMLElement;
	requested: string;
	initialized: boolean;
	appliedId: number;
}

interface CommandJob {
	id: number;
	text: string;
	state: CommandState;
}

const commandStates = new WeakMap<HTMLElement, CommandState>();
const pendingCommands = new Map<HTMLElement, CommandJob>();
let commandWorker: Worker | null = null;
let commandWorkerReady = false;
let commandFailure: Error | null = null;
let activeCommand: CommandJob | null = null;
let commandSequence = 0;
let commandDispatchScheduled = false;

function showCommandFailure(code: HTMLElement, failure: Error): void {
	code.className = "command-cmd";
	code.textContent = `Syntax highlighting failed: ${failure.message}`;
}

function failCommandHighlighter(error: unknown): Error {
	const failure = highlightError(error);
	reportHighlightError(failure);
	if (commandFailure) return commandFailure;
	commandFailure = failure;
	commandWorkerReady = false;
	commandWorker?.terminate();
	commandWorker = null;
	if (activeCommand) showCommandFailure(activeCommand.state.code, failure);
	for (const job of pendingCommands.values()) showCommandFailure(job.state.code, failure);
	activeCommand = null;
	pendingCommands.clear();
	return failure;
}

function connectedCommand(state: CommandState): boolean {
	return state.code.isConnected && state.code.parentElement === state.container;
}

function dispatchCommand(): void {
	if (commandFailure || !commandWorkerReady || activeCommand) return;
	const worker = commandWorker;
	if (!worker) throw new Error("Command highlighting worker is unavailable");
	for (const [code, job] of pendingCommands) {
		pendingCommands.delete(code);
		if (!connectedCommand(job.state)) continue;
		activeCommand = job;
		worker.postMessage({ type: "request", id: job.id, text: job.text } satisfies CommandHighlightRequest);
		return;
	}
}

function scheduleCommand(): void {
	if (commandDispatchScheduled || commandFailure || !commandWorkerReady || activeCommand) return;
	commandDispatchScheduled = true;
	queueMicrotask(() => {
		commandDispatchScheduled = false;
		try {
			dispatchCommand();
		} catch (error) {
			failCommandHighlighter(error);
		}
	});
}

function applyCommandResult(id: number, html: string): void {
	const job = activeCommand;
	if (!job || job.id !== id) throw new Error("Command highlighting response ID mismatch");
	if (connectedCommand(job.state) && id > job.state.appliedId) {
		applyShikiMarkupToCode(job.state.code, parseShikiPre(html));
		job.state.appliedId = id;
	}
	activeCommand = null;
}

/** Start the command worker at application startup. */
export function initCommandHighlighter(): void {
	if (commandWorker || commandFailure) return;
	try {
		const worker = new Worker(new URL("./command-highlight.worker.ts", import.meta.url), { type: "module" });
		commandWorker = worker;
		worker.onmessage = ({ data }: MessageEvent<CommandHighlightResponse>) => {
			if (commandFailure) return;
			try {
				switch (data.type) {
					case "ready":
						if (commandWorkerReady) throw new Error("Command highlighting worker reported ready twice");
						commandWorkerReady = true;
						break;
					case "result":
						applyCommandResult(data.id, data.html);
						break;
					case "error":
						throw new Error(data.message);
					default:
						throw new Error("Unexpected command highlighting worker response");
				}
				dispatchCommand();
			} catch (error) {
				failCommandHighlighter(error);
			}
		};
		worker.onerror = (event) => {
			failCommandHighlighter(new Error(`Command highlighting worker failed: ${event.message}`));
		};
		worker.onmessageerror = () => {
			failCommandHighlighter(new Error("Command highlighting worker message decoding failed"));
		};
	} catch (error) {
		failCommandHighlighter(error);
	}
}

async function initializeCommand(state: CommandState): Promise<void> {
	try {
		await initHighlighter();
		if (!(await ensureLanguageLoaded("bash"))) throw new Error("Bash syntax grammar is unavailable");
		if (commandFailure) {
			showCommandFailure(state.code, commandFailure);
			return;
		}
		if (!connectedCommand(state)) return;
		applyShikiMarkupToCode(state.code, highlightedMarkup(state.requested, "bash"));
		state.initialized = true;
	} catch (error) {
		const failure = failCommandHighlighter(error);
		showCommandFailure(state.code, failure);
	}
}

function createCommandCode(): HTMLElement {
	const code = document.createElement("code");
	code.className = "command-cmd";
	code.setAttribute("data-lang", "bash");
	return code;
}

/** Retain the command node and publish completed highlights in request order. */
export function renderCommand(container: HTMLElement, command: string): void {
	initCommandHighlighter();
	if (commandFailure) {
		const code = createCommandCode();
		showCommandFailure(code, commandFailure);
		container.replaceChildren(code);
		commandStates.delete(container);
		return;
	}
	let state = commandStates.get(container);
	if (!state || state.code.parentElement !== container) {
		const code = createCommandCode();
		code.textContent = command;
		container.replaceChildren(code);
		state = { container, code, requested: command, initialized: false, appliedId: 0 };
		commandStates.set(container, state);
		void initializeCommand(state);
		return;
	}
	if (state.requested === command) return;
	state.requested = command;
	if (!state.initialized) {
		state.code.textContent = command;
		return;
	}
	pendingCommands.set(state.code, { id: ++commandSequence, text: command, state });
	scheduleCommand();
}
