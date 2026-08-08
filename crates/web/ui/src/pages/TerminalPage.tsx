import type { Signal } from "@preact/signals";
import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useCallback, useEffect, useRef } from "preact/hooks";
import * as gon from "../gon";
import { localizedApiErrorMessage } from "../helpers";
import { targetValue } from "../typed-events";

interface ToolsServiceTerminalInfo {
	id: string;
	sessionKey: string;
	running: boolean;
	alive: boolean;
}

interface ToolsServiceInstanceInfo {
	id: string;
	label: string;
	terminals: ToolsServiceTerminalInfo[];
}

interface TerminalSessionInfo {
	id: string;
	instanceId: string;
	sessionKey: string;
	terminals: ToolsServiceTerminalInfo[];
}

interface InstancesResponse {
	instances?: ToolsServiceInstanceInfo[];
	error?: string;
}

interface CreateTerminalResponse {
	terminal?: ToolsServiceTerminalInfo;
	instanceId?: string;
	error?: string;
}

interface SessionTerminalsResponse {
	instanceId?: string;
	terminals?: ToolsServiceTerminalInfo[];
	error?: string;
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
	themeObserver: MutationObserver | null;
	windowResizeListener: (() => void) | null;
	dataDisposable: { dispose: () => void };
	resizeDisposable: { dispose: () => void };
	oscDisposables: { dispose: () => void }[];
	fitFrame: number;
	lastCols: number;
	lastRows: number;
}

interface TerminalViewProps {
	compact: boolean;
	instances: Signal<ToolsServiceInstanceInfo[]>;
	selectedInstanceId: Signal<string>;
	selectedSessionId: Signal<string>;
	selectedTerminalId: Signal<string>;
	sessionKey: Signal<string>;
	status: Signal<string>;
	statusLevel: Signal<"" | "ok" | "error">;
	connected: Signal<boolean>;
	loading: Signal<boolean>;
	creating: Signal<boolean>;
	onRefresh: () => Promise<void>;
	onCreate: () => Promise<void>;
	onSelectSession: (sessionId: string) => void;
	onSelectTerminal: (terminalId: string) => void;
	onControl: (action: "ctrl_c" | "clear" | "restart") => void;
	terminalElementRef: (element: HTMLDivElement | null) => void;
}

interface TerminalPageProps {
	compact?: boolean;
	sessionKey?: string;
}

let terminalContainer: HTMLElement | null = null;
let terminalRuntime: TerminalRuntime | null = null;
let terminalCtor: TerminalCtor | null = null;
let fitAddonCtor: FitAddonCtor | null = null;

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
	const [xtermModule, fitAddonModule] = await Promise.all([import("@xterm/xterm"), import("@xterm/addon-fit")]);
	terminalCtor = (xtermModule as unknown as { Terminal: TerminalCtor }).Terminal;
	fitAddonCtor = (fitAddonModule as unknown as { FitAddon: FitAddonCtor }).FitAddon;
}

function sendSocketMessage(payload: object): boolean {
	const socket = terminalRuntime?.socket;
	if (!socket || socket.readyState !== WebSocket.OPEN) return false;
	socket.send(JSON.stringify(payload));
	return true;
}

function publishTerminalSize(runtime: TerminalRuntime, cols: number, rows: number, force = false): void {
	if (terminalRuntime !== runtime || cols < 2 || rows < 1) return;
	if (!force && cols === runtime.lastCols && rows === runtime.lastRows) return;
	runtime.lastCols = cols;
	runtime.lastRows = rows;
	sendSocketMessage({ type: "resize", cols, rows });
}

function scheduleFit(force = false): void {
	const runtime = terminalRuntime;
	if (!runtime) return;
	if (runtime.fitFrame) cancelAnimationFrame(runtime.fitFrame);
	runtime.fitFrame = requestAnimationFrame(() => {
		runtime.fitFrame = 0;
		runtime.fitAddon.fit();
		publishTerminalSize(runtime, runtime.xterm.cols, runtime.xterm.rows, force);
	});
}

function decodeBase64(encoded: string): Uint8Array | null {
	try {
		const binary = atob(encoded);
		const bytes = new Uint8Array(binary.length);
		for (let index = 0; index < binary.length; index++) bytes[index] = binary.charCodeAt(index) & 0xff;
		return bytes;
	} catch {
		return null;
	}
}

function writeTerminalOutput(data: string | Uint8Array): void {
	const xterm = terminalRuntime?.xterm;
	if (!xterm) return;
	const buffer = xterm.buffer.active;
	const shouldScroll = buffer.baseY - buffer.viewportY <= 2;
	xterm.write(data, () => {
		if (shouldScroll) xterm.scrollToBottom();
	});
}

function closeTerminalRuntime(): void {
	const runtime = terminalRuntime;
	terminalRuntime = null;
	if (!runtime) return;
	if (runtime.fitFrame) cancelAnimationFrame(runtime.fitFrame);
	if (runtime.socket && runtime.socket.readyState < WebSocket.CLOSING) runtime.socket.close();
	runtime.resizeObserver?.disconnect();
	runtime.themeObserver?.disconnect();
	if (runtime.windowResizeListener) window.removeEventListener("resize", runtime.windowResizeListener);
	runtime.dataDisposable.dispose();
	runtime.resizeDisposable.dispose();
	for (const disposable of runtime.oscDisposables) disposable.dispose();
	runtime.xterm.dispose();
}

function closeTerminalSocket(): void {
	const runtime = terminalRuntime;
	const socket = runtime?.socket;
	if (!(runtime && socket)) return;
	runtime.socket = null;
	socket.onmessage = null;
	socket.onclose = null;
	socket.onerror = null;
	if (socket.readyState < WebSocket.CLOSING) socket.close();
}

async function createXterm(element: HTMLDivElement): Promise<TerminalRuntime> {
	await ensureXtermModules();
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
	const oscDisposables = [4, 10, 11, 12, 104, 110, 111, 112].map((code) =>
		xterm.parser.registerOscHandler(code, () => true),
	);
	const dataDisposable = xterm.onData((data) => {
		sendSocketMessage({ type: "input", data });
	});
	let runtime: TerminalRuntime;
	const resizeDisposable = xterm.onResize(({ cols, rows }) => {
		publishTerminalSize(runtime, cols, rows);
	});
	const resizeObserver = typeof ResizeObserver === "undefined" ? null : new ResizeObserver(() => scheduleFit());
	resizeObserver?.observe(element.parentElement ?? element);
	const windowResizeListener = () => scheduleFit();
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
	};
	return runtime;
}

async function readJson<T>(response: Response): Promise<T> {
	try {
		return (await response.json()) as T;
	} catch {
		return {} as T;
	}
}

function terminalLabel(terminal: ToolsServiceTerminalInfo): string {
	const state = terminal.running ? "running" : "idle";
	return `${terminal.id} · ${state}`;
}

function terminalShortId(terminal: ToolsServiceTerminalInfo): string {
	return terminal.id;
}

function terminalSessionId(instanceId: string, sessionKey: string): string {
	return `${encodeURIComponent(instanceId)}:${encodeURIComponent(sessionKey)}`;
}

function terminalSessions(instances: ToolsServiceInstanceInfo[]): TerminalSessionInfo[] {
	const sessions: TerminalSessionInfo[] = [];
	for (const instance of instances) {
		const terminalsBySession = new Map<string, ToolsServiceTerminalInfo[]>();
		for (const terminal of instance.terminals) {
			const terminals = terminalsBySession.get(terminal.sessionKey) ?? [];
			terminals.push(terminal);
			terminalsBySession.set(terminal.sessionKey, terminals);
		}
		for (const [sessionKey, terminals] of terminalsBySession) {
			sessions.push({
				id: terminalSessionId(instance.id, sessionKey),
				instanceId: instance.id,
				sessionKey,
				terminals,
			});
		}
	}
	return sessions;
}

function CompactTerminalView({
	props,
	selectedSession,
}: {
	props: TerminalViewProps;
	selectedSession: TerminalSessionInfo | null;
}): VNode {
	return (
		<div className="terminal-page chat-terminal-page">
			<div className="terminal-tabs-bar chat-terminal-tabs-bar">
				<nav className="terminal-tabs chat-terminal-tabs" aria-label="Chat terminals">
					{selectedSession?.terminals.map((terminal) => {
						const state = terminal.running ? "running" : "idle";
						return (
							<button
								key={terminal.id}
								type="button"
								className={`terminal-tab chat-terminal-tab ${terminal.id === props.selectedTerminalId.value ? "active" : ""}`}
								title={`Terminal ${terminalShortId(terminal)} · ${state}`}
								aria-label={`Terminal ${terminalShortId(terminal)}, ${state}`}
								onClick={() => {
									props.onSelectTerminal(terminal.id);
								}}
							>
								<span>{terminalShortId(terminal)}</span>
								<span className={`chat-terminal-state chat-terminal-state-${state}`} aria-hidden="true" />
							</button>
						);
					})}
					<button
						type="button"
						className="terminal-tab chat-terminal-new-tab"
						title="New terminal tab"
						aria-label="New terminal tab"
						disabled={props.creating.value || props.loading.value}
						onClick={props.onCreate}
					>
						+
					</button>
				</nav>
			</div>
			<div className="terminal-output-wrap chat-terminal-output-wrap">
				<div
					ref={props.terminalElementRef}
					className="terminal-output chat-terminal-output"
					role="log"
					aria-label="Chat terminal output"
				/>
			</div>
			{props.statusLevel.value === "error" && (
				<div className="terminal-status terminal-status-error chat-terminal-status" role="alert">
					{props.status.value}
				</div>
			)}
		</div>
	);
}

function FullTerminalView({
	props,
	sessions,
	selectedSession,
	selectedInstance,
	selectedTerminal,
}: {
	props: TerminalViewProps;
	sessions: TerminalSessionInfo[];
	selectedSession: TerminalSessionInfo | null;
	selectedInstance: ToolsServiceInstanceInfo | null;
	selectedTerminal: ToolsServiceTerminalInfo | null;
}): VNode {
	return (
		<div className="terminal-page">
			<div className="terminal-toolbar">
				<div className="terminal-heading">
					<h2 className="text-lg font-medium text-[var(--text-strong)]">Terminal</h2>
					<div className="terminal-meta">Real terminals owned by the active tools service</div>
				</div>
				<div className="terminal-actions">
					<button className="logs-btn" type="button" disabled={props.loading.value} onClick={props.onRefresh}>
						Refresh
					</button>
					<button
						className="logs-btn"
						type="button"
						disabled={!props.connected.value}
						onClick={() => props.onControl("ctrl_c")}
					>
						Ctrl+C
					</button>
					<button
						className="logs-btn"
						type="button"
						disabled={!props.connected.value}
						onClick={() => props.onControl("clear")}
					>
						Clear
					</button>
					<button
						className="logs-btn"
						type="button"
						disabled={!props.connected.value}
						onClick={() => props.onControl("restart")}
					>
						Restart attachment
					</button>
				</div>
			</div>

			<div className="terminal-tabs-bar gap-2">
				<label className="sr-only" htmlFor="terminalSession">
					Agent session
				</label>
				<select
					id="terminalSession"
					className="logs-btn max-w-64"
					value={props.selectedSessionId.value}
					disabled={sessions.length === 0}
					onChange={(event) => {
						props.onSelectSession(targetValue(event));
					}}
				>
					{sessions.map((session) => (
						<option key={session.id} value={session.id}>
							{session.sessionKey}
						</option>
					))}
				</select>
				<nav className="terminal-tabs" aria-label="Managed terminals">
					{selectedSession?.terminals.map((terminal) => (
						<button
							key={terminal.id}
							type="button"
							className={`terminal-tab ${terminal.id === props.selectedTerminalId.value ? "active" : ""}`}
							title={`Attach terminal ${terminal.id}`}
							onClick={() => {
								props.onSelectTerminal(terminal.id);
							}}
						>
							{terminalLabel(terminal)}
						</button>
					))}
					{selectedSession && selectedSession.terminals.length === 0 ? (
						<span className="terminal-tab-empty">No managed terminals</span>
					) : null}
				</nav>
			</div>

			<div className="flex flex-wrap items-end gap-2 px-3 py-2">
				<label className="flex min-w-64 flex-1 flex-col gap-1 text-xs text-[var(--muted)]" htmlFor="terminalSessionKey">
					Session key for a new terminal
					<input
						id="terminalSessionKey"
						className="logs-input font-mono"
						type="text"
						value={props.sessionKey.value}
						placeholder="Enter an explicit agent session key"
						onInput={(event) => {
							props.sessionKey.value = targetValue(event);
						}}
					/>
				</label>
				<button
					className="logs-btn"
					type="button"
					disabled={!selectedInstance || props.creating.value || props.sessionKey.value.trim().length === 0}
					onClick={props.onCreate}
				>
					{props.creating.value ? "Creating…" : "Create in selected service"}
				</button>
			</div>

			{selectedTerminal ? (
				<div className="grid grid-cols-2 gap-x-4 gap-y-1 px-3 pb-2 font-mono text-xs text-[var(--muted)]">
					<span>terminal: {selectedTerminal.id}</span>
					<span>state: {selectedTerminal.alive ? (selectedTerminal.running ? "running" : "ready") : "exited"}</span>
					<span className="col-span-2">session key: {selectedTerminal.sessionKey}</span>
				</div>
			) : null}

			<div className="terminal-output-wrap">
				<div
					ref={props.terminalElementRef}
					className="terminal-output"
					role="log"
					aria-label="Managed terminal output"
				/>
			</div>
			<div
				className={`terminal-status ${props.statusLevel.value === "error" ? "terminal-status-error" : ""} ${props.statusLevel.value === "ok" ? "terminal-status-ok" : ""}`}
			>
				{props.status.value}
			</div>
			<div className="terminal-hint">
				Inventory, retained history, and live attachment are owned by the selected in-process terminal service.
			</div>
		</div>
	);
}

function TerminalView(props: TerminalViewProps): VNode {
	const sessions = terminalSessions(props.instances.value);
	const selectedSession = sessions.find((session) => session.id === props.selectedSessionId.value) ?? null;
	if (props.compact) return <CompactTerminalView props={props} selectedSession={selectedSession} />;
	const selectedInstance =
		props.instances.value.find((instance) => instance.id === props.selectedInstanceId.value) ?? null;
	const selectedTerminal =
		selectedSession?.terminals.find((terminal) => terminal.id === props.selectedTerminalId.value) ?? null;
	return (
		<FullTerminalView
			props={props}
			sessions={sessions}
			selectedSession={selectedSession}
			selectedInstance={selectedInstance}
			selectedTerminal={selectedTerminal}
		/>
	);
}

interface TerminalInventorySelection {
	instanceId: string;
	sessionId: string;
	terminalId: string;
	terminalChanged: boolean;
}

interface CreatedTerminal {
	terminal: ToolsServiceTerminalInfo;
	instanceId?: string;
}

interface TerminalSocketContext {
	runtime: TerminalRuntime;
	socket: WebSocket;
	terminal: ToolsServiceTerminalInfo;
	connected: Signal<boolean>;
	status: Signal<string>;
	statusLevel: Signal<"" | "ok" | "error">;
}

type ParsedTerminalMessage =
	| { kind: "ignore" }
	| { kind: "invalid" }
	| { kind: "message"; message: TerminalServerMessage };

async function fetchTerminalInventory(compact: boolean, sessionKey: string): Promise<ToolsServiceInstanceInfo[]> {
	const url = compact
		? `/api/terminal/terminals?${new URLSearchParams({ sessionKey }).toString()}`
		: "/api/terminal/instances";
	const response = await fetch(url, { headers: { Accept: "application/json" } });
	if (compact) {
		const payload = await readJson<SessionTerminalsResponse>(response);
		if (!response.ok) throw new Error(localizedApiErrorMessage(payload as never, "Failed to load terminals"));
		if (!payload.instanceId) return [];
		return [
			{
				id: payload.instanceId,
				label: "",
				terminals: Array.isArray(payload.terminals) ? payload.terminals : [],
			},
		];
	}
	const payload = await readJson<InstancesResponse>(response);
	if (!response.ok) throw new Error(localizedApiErrorMessage(payload as never, "Failed to load terminals"));
	return Array.isArray(payload.instances) ? payload.instances : [];
}

function terminalInventorySelection(
	instances: ToolsServiceInstanceInfo[],
	currentInstanceId: string,
	currentSessionId: string,
	currentTerminalId: string,
): TerminalInventorySelection {
	const sessions = terminalSessions(instances);
	const session = sessions.find((candidate) => candidate.id === currentSessionId) ?? sessions[0] ?? null;
	const terminalAvailable = session?.terminals.some((terminal) => terminal.id === currentTerminalId) === true;
	return {
		sessionId: session?.id ?? "",
		instanceId:
			session?.instanceId ??
			instances.find((instance) => instance.id === currentInstanceId)?.id ??
			instances[0]?.id ??
			"",
		terminalId: terminalAvailable ? currentTerminalId : (session?.terminals[0]?.id ?? ""),
		terminalChanged: !terminalAvailable,
	};
}

function terminalInventoryStatus(
	compact: boolean,
	instanceCount: number,
): { text: string; level: "" | "ok" | "error" } {
	if (compact) return { text: "", level: "" };
	if (instanceCount === 0) return { text: "No active tools service instances are registered.", level: "error" };
	return { text: "Inventory refreshed.", level: "ok" };
}

async function requestTerminalCreation(
	compact: boolean,
	instanceId: string,
	sessionKey: string,
): Promise<CreatedTerminal> {
	const url = compact
		? "/api/terminal/terminals"
		: `/api/terminal/instances/${encodeURIComponent(instanceId)}/terminals`;
	const response = await fetch(url, {
		method: "POST",
		headers: { Accept: "application/json", "Content-Type": "application/json" },
		body: JSON.stringify({ sessionKey }),
	});
	const payload = await readJson<CreateTerminalResponse>(response);
	if (!(response.ok && payload.terminal)) {
		throw new Error(localizedApiErrorMessage(payload as never, "Failed to create terminal"));
	}
	return { terminal: payload.terminal, instanceId: payload.instanceId };
}

function parseTerminalMessage(data: unknown): ParsedTerminalMessage {
	if (typeof data !== "string") return { kind: "ignore" };
	try {
		return { kind: "message", message: JSON.parse(data) as TerminalServerMessage };
	} catch {
		return { kind: "invalid" };
	}
}

function handleTerminalReady(context: TerminalSocketContext, message: TerminalServerMessage): void {
	if (!message.available || message.terminal?.id !== context.terminal.id) {
		context.status.value = "Tools service returned mismatched terminal metadata.";
		context.statusLevel.value = "error";
		context.socket.close();
		return;
	}
	context.connected.value = true;
	context.status.value = `Attached to exact terminal ${context.terminal.id}.`;
	context.statusLevel.value = "ok";
	scheduleFit(true);
	context.runtime.xterm.focus();
}

function handleTerminalOutput(message: TerminalServerMessage): void {
	const output = message.encoding === "base64" ? decodeBase64(message.data ?? "") : (message.data ?? "");
	if (output !== null) writeTerminalOutput(output);
}

function handleTerminalStatus(context: TerminalSocketContext, message: TerminalServerMessage): void {
	context.status.value = message.text ?? message.error ?? "Terminal error";
	context.statusLevel.value = message.level === "error" || message.type === "error" ? "error" : "";
}

function handleTerminalSocketMessage(context: TerminalSocketContext, event: MessageEvent<unknown>): void {
	if (context.runtime.socket !== context.socket) return;
	const parsed = parseTerminalMessage(event.data);
	if (parsed.kind === "ignore") return;
	if (parsed.kind === "invalid") {
		context.status.value = "Invalid terminal message received.";
		context.statusLevel.value = "error";
		return;
	}
	if (parsed.message.type === "ready") handleTerminalReady(context, parsed.message);
	else if (parsed.message.type === "output") handleTerminalOutput(parsed.message);
	else if (parsed.message.type === "status" || parsed.message.type === "error") {
		handleTerminalStatus(context, parsed.message);
	}
}

function TerminalPage({ compact = false, sessionKey: fixedSessionKey }: TerminalPageProps): VNode {
	const instances = useSignal<ToolsServiceInstanceInfo[]>([]);
	const selectedInstanceId = useSignal("");
	const selectedSessionId = useSignal("");
	const selectedTerminalId = useSignal("");
	const sessionKey = useSignal(fixedSessionKey ?? "");
	const status = useSignal("Loading tools service terminal inventory…");
	const statusLevel = useSignal<"" | "ok" | "error">("");
	const connected = useSignal(false);
	const loading = useSignal(false);
	const creating = useSignal(false);
	const terminalElementRef = useRef<HTMLDivElement | null>(null);
	const initializedElement = useRef<HTMLDivElement | null>(null);
	const setTerminalElementRef = useCallback((element: HTMLDivElement | null) => {
		terminalElementRef.current = element;
	}, []);

	function selectedTerminal(): ToolsServiceTerminalInfo | null {
		const instance = instances.value.find((candidate) => candidate.id === selectedInstanceId.value);
		const session = terminalSessions(instances.value).find((candidate) => candidate.id === selectedSessionId.value);
		return (
			instance?.terminals.find(
				(terminal) => terminal.sessionKey === session?.sessionKey && terminal.id === selectedTerminalId.value,
			) ?? null
		);
	}

	function selectSession(sessionId: string): void {
		const session = terminalSessions(instances.value).find((candidate) => candidate.id === sessionId) ?? null;
		closeTerminalSocket();
		connected.value = false;
		selectedSessionId.value = session?.id ?? "";
		selectedInstanceId.value = session?.instanceId ?? "";
		selectedTerminalId.value = session?.terminals[0]?.id ?? "";
		connect();
	}

	function selectTerminal(terminalId: string): void {
		if (terminalId === selectedTerminalId.value && connected.value) {
			terminalRuntime?.xterm.focus();
			return;
		}
		closeTerminalSocket();
		connected.value = false;
		selectedTerminalId.value = terminalId;
		connect();
	}

	async function refreshInventory(): Promise<void> {
		loading.value = true;
		try {
			const nextInstances = await fetchTerminalInventory(compact, sessionKey.value);
			const selection = terminalInventorySelection(
				nextInstances,
				selectedInstanceId.value,
				selectedSessionId.value,
				selectedTerminalId.value,
			);
			instances.value = nextInstances;
			selectedSessionId.value = selection.sessionId;
			selectedInstanceId.value = selection.instanceId;
			if (selection.terminalChanged) {
				closeTerminalSocket();
				selectedTerminalId.value = selection.terminalId;
				connected.value = false;
			}
			const nextStatus = terminalInventoryStatus(compact, nextInstances.length);
			status.value = nextStatus.text;
			statusLevel.value = nextStatus.level;
			if (selectedTerminalId.value) connect();
		} catch (error) {
			closeTerminalSocket();
			instances.value = [];
			selectedInstanceId.value = "";
			selectedSessionId.value = "";
			selectedTerminalId.value = "";
			connected.value = false;
			status.value = error instanceof Error ? error.message : "Failed to load terminals";
			statusLevel.value = "error";
		} finally {
			loading.value = false;
		}
	}

	async function createTerminal(): Promise<void> {
		const explicitSessionKey = sessionKey.value.trim();
		if (!(explicitSessionKey && (compact || selectedInstanceId.value))) return;
		creating.value = true;
		try {
			const created = await requestTerminalCreation(compact, selectedInstanceId.value, explicitSessionKey);
			await refreshInventory();
			if (created.instanceId) selectedInstanceId.value = created.instanceId;
			selectedSessionId.value = terminalSessionId(selectedInstanceId.value, created.terminal.sessionKey);
			selectedTerminalId.value = created.terminal.id;
			status.value = `Created exact terminal ${created.terminal.id}.`;
			statusLevel.value = "ok";
			connect();
		} catch (error) {
			status.value = error instanceof Error ? error.message : "Failed to create terminal";
			statusLevel.value = "error";
		} finally {
			creating.value = false;
		}
	}

	function connect(): void {
		const terminal = selectedTerminal();
		const runtime = terminalRuntime;
		if (!(terminal && runtime && selectedInstanceId.value)) return;
		closeTerminalSocket();
		runtime.xterm.reset();
		connected.value = false;
		const protocol = location.protocol === "https:" ? "wss:" : "ws:";
		const query = new URLSearchParams({
			instanceId: selectedInstanceId.value,
			id: terminal.id,
			sessionKey: terminal.sessionKey,
		});
		const socket = new WebSocket(`${protocol}//${location.host}/api/terminal/ws?${query.toString()}`);
		runtime.socket = socket;
		status.value = `Connecting to exact terminal ${terminal.id}…`;
		statusLevel.value = "";
		const socketContext: TerminalSocketContext = {
			runtime,
			socket,
			terminal,
			connected,
			status,
			statusLevel,
		};
		socket.onmessage = (event) => handleTerminalSocketMessage(socketContext, event);
		socket.onclose = () => {
			if (runtime.socket !== socket) return;
			runtime.socket = null;
			connected.value = false;
			if (statusLevel.value !== "error") {
				status.value = `Terminal ${terminal.id} disconnected.`;
				statusLevel.value = "";
			}
		};
		socket.onerror = () => {
			if (runtime.socket !== socket) return;
			status.value = `Failed to attach terminal ${terminal.id}.`;
			statusLevel.value = "error";
		};
	}

	function control(action: "ctrl_c" | "clear" | "restart"): void {
		if (action === "restart") {
			connect();
			return;
		}
		if (!sendSocketMessage({ type: "control", action })) {
			status.value = "Terminal is not connected.";
			statusLevel.value = "error";
		}
	}

	useEffect(() => {
		const element = terminalElementRef.current;
		if (!element || initializedElement.current === element) return;
		initializedElement.current = element;
		void createXterm(element)
			.then((runtime) => {
				terminalRuntime = runtime;
				scheduleFit();
				if (selectedTerminal()) connect();
			})
			.catch((error: unknown) => {
				status.value = error instanceof Error ? error.message : "Failed to initialize xterm";
				statusLevel.value = "error";
			});
	}, []);

	useEffect(() => {
		void refreshInventory();
		return () => closeTerminalRuntime();
	}, []);

	return (
		<TerminalView
			compact={compact}
			instances={instances}
			selectedInstanceId={selectedInstanceId}
			selectedSessionId={selectedSessionId}
			selectedTerminalId={selectedTerminalId}
			sessionKey={sessionKey}
			status={status}
			statusLevel={statusLevel}
			connected={connected}
			loading={loading}
			creating={creating}
			onRefresh={refreshInventory}
			onCreate={createTerminal}
			onSelectSession={selectSession}
			onSelectTerminal={selectTerminal}
			onControl={control}
			terminalElementRef={setTerminalElementRef}
		/>
	);
}

export function initTerminal(container: HTMLElement): void {
	terminalContainer = container;
	container.classList.add("flex", "min-h-0", "flex-col", "overflow-hidden", "p-0");
	render(<TerminalPage />, container);
}

export function teardownTerminal(): void {
	closeTerminalRuntime();
	if (terminalContainer) {
		render(null, terminalContainer);
		terminalContainer.classList.remove("flex", "min-h-0", "flex-col", "overflow-hidden", "p-0");
	}
	terminalContainer = null;
}

export function initChatTerminal(container: HTMLElement, sessionKey: string): void {
	terminalContainer = container;
	render(<TerminalPage compact sessionKey={sessionKey} />, container);
}

export function teardownChatTerminal(): void {
	closeTerminalRuntime();
	if (terminalContainer) render(null, terminalContainer);
	terminalContainer = null;
}
