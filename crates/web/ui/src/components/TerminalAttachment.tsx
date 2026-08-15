import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import * as gon from "../gon";

export interface ToolsServiceTerminalInfo {
	id: string;
	sessionKey: string;
	running: boolean;
	alive: boolean;
}

export type TerminalConnection =
	| {
			mode: "terminal";
			instanceId: string;
			terminalId: string;
			sessionKey: string;
	  }
	| {
			mode: "tool_call";
			toolCallId: string;
			sessionKey: string;
	  };

export interface TerminalAttachmentController {
	control: (action: "ctrl_c" | "clear") => boolean;
	focus: () => void;
	restart: () => void;
}

interface TerminalAttachmentProps {
	connection: TerminalConnection | null;
	className: string;
	ariaLabel: string;
	focusOnReady?: boolean;
	onConnectedChange?: (connected: boolean) => void;
	onController?: (controller: TerminalAttachmentController | null) => void;
	onReady?: (terminal: ToolsServiceTerminalInfo) => void;
	onStatus?: (text: string, level: "" | "ok" | "error") => void;
}

interface XtermOptions {
	convertEol?: boolean;
	disableStdin?: boolean;
	cursorBlink?: boolean;
	scrollback?: number;
	fontFamily?: string;
	fontSize?: number;
	lineHeight?: number;
	theme?: Record<string, string>;
}

interface XtermInstance {
	cols: number;
	rows: number;
	options: { theme?: Record<string, string>; [key: string]: unknown };
	buffer: { active: { baseY: number; viewportY: number } };
	parser: { registerOscHandler: (code: number, handler: () => boolean) => { dispose: () => void } };
	loadAddon: (addon: FitAddonInstance) => void;
	open: (element: HTMLElement) => void;
	onData: (handler: (data: string) => void) => { dispose: () => void };
	onResize: (handler: (size: { cols: number; rows: number }) => void) => { dispose: () => void };
	write: (data: string | Uint8Array, callback?: () => void) => void;
	reset: () => void;
	focus: () => void;
	scrollToBottom: () => void;
	dispose: () => void;
}

interface FitAddonInstance {
	fit: () => void;
}

type TerminalCtor = new (options: XtermOptions) => XtermInstance;
type FitAddonCtor = new () => FitAddonInstance;

interface TerminalRuntime {
	xterm: XtermInstance;
	fitAddon: FitAddonInstance;
	socket: WebSocket | null;
	resizeObserver: ResizeObserver | null;
	themeObserver: MutationObserver;
	windowResizeListener: () => void;
	dataDisposable: { dispose: () => void };
	resizeDisposable: { dispose: () => void };
	oscDisposables: { dispose: () => void }[];
	fitFrame: number;
	lastCols: number;
	lastRows: number;
	ready: boolean;
	disposed: boolean;
}

interface TerminalServerMessage {
	type: string;
	available?: boolean;
	data?: string;
	encoding?: string;
	text?: string;
	level?: string;
	error?: string;
	terminal?: ToolsServiceTerminalInfo;
}

type TerminalSocketCallbacks = Pick<TerminalAttachmentProps, "onConnectedChange" | "onReady" | "onStatus">;

interface TerminalSocketState {
	attachedTerminal: ToolsServiceTerminalInfo | null;
	preserveStatusOnClose: boolean;
}

interface TerminalSocketContext {
	runtime: TerminalRuntime;
	socket: WebSocket;
	connection: TerminalConnection;
	state: TerminalSocketState;
	callbacks: TerminalSocketCallbacks;
	focusOnReady: boolean;
}

let terminalCtor: TerminalCtor | null = null;
let fitAddonCtor: FitAddonCtor | null = null;
let xtermModulesPromise: Promise<void> | null = null;

function getCssVar(name: string, fallback: string): string {
	return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

function xtermTheme(): Record<string, string> {
	return {
		background: getCssVar("--bg", "#0f1115"),
		foreground: getCssVar("--text", "#e4e4e7"),
		cursor: getCssVar("--accent", "#4ade80"),
		cursorAccent: getCssVar("--bg", "#0f1115"),
		selectionBackground: getCssVar("--accent-subtle", "#4ade801f"),
	};
}

async function ensureXtermModules(): Promise<void> {
	if (terminalCtor && fitAddonCtor) return;
	xtermModulesPromise ??= Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]).then(
		([xtermModule, fitAddonModule]) => {
			terminalCtor = (xtermModule as unknown as { Terminal: TerminalCtor }).Terminal;
			fitAddonCtor = (fitAddonModule as unknown as { FitAddon: FitAddonCtor }).FitAddon;
		},
	);
	await xtermModulesPromise;
}

function sendSocketMessage(runtime: TerminalRuntime, payload: object): boolean {
	const socket = runtime.socket;
	if (!(runtime.ready && socket?.readyState === WebSocket.OPEN)) return false;
	socket.send(JSON.stringify(payload));
	return true;
}

function publishTerminalSize(runtime: TerminalRuntime, cols: number, rows: number, force = false): void {
	if (runtime.disposed || cols < 2 || rows < 1) return;
	if (!force && cols === runtime.lastCols && rows === runtime.lastRows) return;
	if (!sendSocketMessage(runtime, { type: "resize", cols, rows })) return;
	runtime.lastCols = cols;
	runtime.lastRows = rows;
}

function scheduleFit(runtime: TerminalRuntime, force = false): void {
	if (runtime.disposed) return;
	if (runtime.fitFrame) cancelAnimationFrame(runtime.fitFrame);
	runtime.fitFrame = requestAnimationFrame(() => {
		runtime.fitFrame = 0;
		if (runtime.disposed) return;
		runtime.fitAddon.fit();
		publishTerminalSize(runtime, runtime.xterm.cols, runtime.xterm.rows, force);
	});
}

function decodeBase64(encoded: string): Uint8Array | null {
	try {
		const binary = atob(encoded);
		const bytes = new Uint8Array(binary.length);
		for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index) & 0xff;
		return bytes;
	} catch {
		return null;
	}
}

function writeTerminalOutput(runtime: TerminalRuntime, data: string | Uint8Array): void {
	const buffer = runtime.xterm.buffer.active;
	const shouldScroll = buffer.baseY - buffer.viewportY <= 2;
	runtime.xterm.write(data, () => {
		if (shouldScroll && !runtime.disposed) runtime.xterm.scrollToBottom();
	});
}

function closeSocket(runtime: TerminalRuntime): void {
	const socket = runtime.socket;
	runtime.socket = null;
	runtime.ready = false;
	if (!socket) return;
	socket.onmessage = null;
	socket.onclose = null;
	socket.onerror = null;
	if (socket.readyState < WebSocket.CLOSING) socket.close();
}

function disposeRuntime(runtime: TerminalRuntime): void {
	if (runtime.disposed) return;
	runtime.disposed = true;
	closeSocket(runtime);
	if (runtime.fitFrame) cancelAnimationFrame(runtime.fitFrame);
	runtime.resizeObserver?.disconnect();
	runtime.themeObserver.disconnect();
	window.removeEventListener("resize", runtime.windowResizeListener);
	runtime.dataDisposable.dispose();
	runtime.resizeDisposable.dispose();
	for (const disposable of runtime.oscDisposables) disposable.dispose();
	runtime.xterm.dispose();
}

async function createRuntime(element: HTMLDivElement, signal: AbortSignal): Promise<TerminalRuntime> {
	await ensureXtermModules();
	if (signal.aborted) throw signal.reason;
	if (!(terminalCtor && fitAddonCtor)) throw new Error("xterm failed to load");
	const scrollbackLines = gon.get("terminal_scrollback_lines");
	if (typeof scrollbackLines !== "number" || !Number.isSafeInteger(scrollbackLines) || scrollbackLines < 0) {
		throw new Error("terminal scrollback configuration is unavailable or invalid");
	}
	const xterm = new terminalCtor({
		convertEol: false,
		disableStdin: false,
		cursorBlink: true,
		scrollback: scrollbackLines,
		fontFamily: "JetBrains Mono, ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, monospace",
		fontSize: 12,
		lineHeight: 1.35,
		theme: xtermTheme(),
	});
	const fitAddon = new fitAddonCtor();
	xterm.loadAddon(fitAddon);
	xterm.open(element);
	let runtime: TerminalRuntime | null = null;
	const oscDisposables = [4, 10, 11, 12, 104, 110, 111, 112].map((code) =>
		xterm.parser.registerOscHandler(code, () => true),
	);
	const dataDisposable = xterm.onData((data) => {
		if (runtime) sendSocketMessage(runtime, { type: "input", data });
	});
	const resizeDisposable = xterm.onResize(({ cols, rows }) => {
		if (runtime) publishTerminalSize(runtime, cols, rows);
	});
	const resizeObserver =
		typeof ResizeObserver === "undefined"
			? null
			: new ResizeObserver(() => {
					if (runtime) scheduleFit(runtime);
				});
	resizeObserver?.observe(element.parentElement ?? element);
	const windowResizeListener = () => {
		if (runtime) scheduleFit(runtime);
	};
	window.addEventListener("resize", windowResizeListener);
	const themeObserver = new MutationObserver(() => {
		xterm.options.theme = xtermTheme();
	});
	themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
	runtime = {
		xterm,
		fitAddon,
		socket: null,
		resizeObserver,
		themeObserver,
		windowResizeListener,
		dataDisposable,
		resizeDisposable,
		oscDisposables,
		fitFrame: 0,
		lastCols: 0,
		lastRows: 0,
		ready: false,
		disposed: false,
	};
	return runtime;
}

function connectionUrl(connection: TerminalConnection): string {
	const protocol = location.protocol === "https:" ? "wss:" : "ws:";
	const query =
		connection.mode === "terminal"
			? new URLSearchParams({
					instanceId: connection.instanceId,
					id: connection.terminalId,
					sessionKey: connection.sessionKey,
				})
			: new URLSearchParams({
					toolCallId: connection.toolCallId,
					sessionKey: connection.sessionKey,
				});
	return `${protocol}//${location.host}/api/terminal/ws?${query.toString()}`;
}

function terminalMatchesConnection(terminal: ToolsServiceTerminalInfo, connection: TerminalConnection): boolean {
	if (terminal.sessionKey !== connection.sessionKey) return false;
	return connection.mode === "tool_call" || terminal.id === connection.terminalId;
}

function parseTerminalMessage(data: unknown): TerminalServerMessage | null {
	if (typeof data !== "string") return null;
	try {
		const parsed: unknown = JSON.parse(data);
		return parsed && typeof parsed === "object" ? (parsed as TerminalServerMessage) : null;
	} catch {
		return null;
	}
}

function rejectTerminalMessage(context: TerminalSocketContext, message: string): void {
	context.state.preserveStatusOnClose = true;
	context.callbacks.onStatus?.(message, "error");
	context.socket.close();
}

function handleTerminalReadyMessage(message: TerminalServerMessage, context: TerminalSocketContext): void {
	const terminal = message.terminal;
	if (!(message.available && terminal && terminalMatchesConnection(terminal, context.connection))) {
		rejectTerminalMessage(context, "Tools service returned mismatched terminal metadata.");
		return;
	}
	context.state.attachedTerminal = terminal;
	context.runtime.ready = true;
	context.callbacks.onConnectedChange?.(true);
	context.callbacks.onReady?.(terminal);
	context.callbacks.onStatus?.(`Attached to exact terminal ${terminal.id}.`, "ok");
	scheduleFit(context.runtime, true);
	if (context.focusOnReady) context.runtime.xterm.focus();
}

function handleTerminalOutputMessage(message: TerminalServerMessage, context: TerminalSocketContext): void {
	if (!context.runtime.ready) {
		rejectTerminalMessage(context, "Terminal output arrived before attachment was ready.");
		return;
	}
	const output = message.encoding === "base64" ? decodeBase64(message.data ?? "") : (message.data ?? "");
	if (output === null) {
		rejectTerminalMessage(context, "Invalid terminal output encoding.");
		return;
	}
	writeTerminalOutput(context.runtime, output);
}

function handleTerminalStatusMessage(message: TerminalServerMessage, context: TerminalSocketContext): void {
	const level: "" | "error" = message.level === "error" || message.type === "error" ? "error" : "";
	if (level === "error") context.state.preserveStatusOnClose = true;
	context.callbacks.onStatus?.(message.text ?? message.error ?? "Terminal error", level);
}

function handleTerminalServerMessage(message: TerminalServerMessage, context: TerminalSocketContext): void {
	switch (message.type) {
		case "ready":
			handleTerminalReadyMessage(message, context);
			return;
		case "output":
			handleTerminalOutputMessage(message, context);
			return;
		case "status":
		case "error":
			handleTerminalStatusMessage(message, context);
	}
}

export function TerminalAttachment({
	connection,
	className,
	ariaLabel,
	focusOnReady = true,
	onConnectedChange,
	onController,
	onReady,
	onStatus,
}: TerminalAttachmentProps): VNode {
	const elementRef = useRef<HTMLDivElement | null>(null);
	const callbacksRef = useRef({ onConnectedChange, onController, onReady, onStatus });
	callbacksRef.current = { onConnectedChange, onController, onReady, onStatus };
	const runtimeSignal = useSignal<TerminalRuntime | null>(null);
	const [restartRevision, setRestartRevision] = useState(0);

	useEffect(() => {
		const element = elementRef.current;
		if (!element) return;
		let cancelled = false;
		const abortController = new AbortController();
		void createRuntime(element, abortController.signal)
			.then((runtime) => {
				if (cancelled) {
					disposeRuntime(runtime);
					return;
				}
				runtimeSignal.value = runtime;
				scheduleFit(runtime);
				callbacksRef.current.onController?.({
					control: (action) => sendSocketMessage(runtime, { type: "control", action }),
					focus: () => runtime.xterm.focus(),
					restart: () => setRestartRevision((revision) => revision + 1),
				});
			})
			.catch((error: unknown) => {
				if (cancelled) return;
				callbacksRef.current.onStatus?.(error instanceof Error ? error.message : "Failed to initialize xterm", "error");
			});
		return () => {
			cancelled = true;
			abortController.abort(new Error("terminal attachment initialization cancelled"));
			callbacksRef.current.onController?.(null);
			const runtime = runtimeSignal.peek();
			if (runtime) disposeRuntime(runtime);
			runtimeSignal.value = null;
		};
	}, []);

	useEffect(() => {
		const runtime = runtimeSignal.value;
		if (!(runtime && connection)) return;
		closeSocket(runtime);
		runtime.xterm.reset();
		callbacksRef.current.onConnectedChange?.(false);
		callbacksRef.current.onStatus?.(
			connection.mode === "terminal"
				? `Connecting to exact terminal ${connection.terminalId}…`
				: "Connecting to terminal…",
			"",
		);
		const socket = new WebSocket(connectionUrl(connection));
		const socketState: TerminalSocketState = {
			attachedTerminal: null,
			preserveStatusOnClose: false,
		};
		runtime.socket = socket;
		socket.onmessage = (event: MessageEvent<unknown>) => {
			if (runtime.socket !== socket) return;
			const context: TerminalSocketContext = {
				runtime,
				socket,
				connection,
				state: socketState,
				callbacks: callbacksRef.current,
				focusOnReady,
			};
			const message = parseTerminalMessage(event.data);
			if (!message) {
				rejectTerminalMessage(context, "Invalid terminal message received.");
				return;
			}
			handleTerminalServerMessage(message, context);
		};
		socket.onclose = () => {
			if (runtime.socket !== socket) return;
			runtime.socket = null;
			runtime.ready = false;
			callbacksRef.current.onConnectedChange?.(false);
			if (!socketState.preserveStatusOnClose && connection.mode === "terminal") {
				callbacksRef.current.onStatus?.(
					`Terminal ${socketState.attachedTerminal?.id ?? connection.terminalId} disconnected.`,
					"",
				);
			}
		};
		socket.onerror = () => {
			if (runtime.socket !== socket) return;
			socketState.preserveStatusOnClose = true;
			callbacksRef.current.onStatus?.(
				connection.mode === "terminal"
					? `Failed to attach terminal ${connection.terminalId}.`
					: "Failed to attach terminal.",
				"error",
			);
		};
		return () => {
			if (runtime.socket === socket) closeSocket(runtime);
			callbacksRef.current.onConnectedChange?.(false);
		};
	}, [
		connection?.mode,
		connection?.sessionKey,
		connection?.mode === "terminal" ? connection.instanceId : "",
		connection?.mode === "terminal" ? connection.terminalId : "",
		connection?.mode === "tool_call" ? connection.toolCallId : "",
		restartRevision,
		runtimeSignal.value,
	]);

	return <div ref={elementRef} className={className} role="log" aria-label={ariaLabel} />;
}
