import type { Signal } from "@preact/signals";
import { useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useRef } from "preact/hooks";
import {
	TerminalAttachment,
	type TerminalAttachmentController,
	type TerminalConnection,
	type ToolsServiceTerminalInfo,
} from "../components/TerminalAttachment";
import { localizedApiErrorMessage } from "../helpers";
import { targetValue } from "../typed-events";

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
	connection: TerminalConnection | null;
	onRefresh: () => Promise<void>;
	onCreate: () => Promise<void>;
	onSelectSession: (sessionId: string) => void;
	onSelectTerminal: (terminalId: string) => void;
	onControl: (action: "ctrl_c" | "clear" | "restart") => void;
	onConnectedChange: (connected: boolean) => void;
	onController: (controller: TerminalAttachmentController | null) => void;
	onStatus: (text: string, level: "" | "ok" | "error") => void;
}

interface TerminalPageProps {
	compact?: boolean;
	sessionKey?: string;
}

interface TerminalInventorySelection {
	instanceId: string;
	sessionId: string;
	terminalId: string;
}

interface CreatedTerminal {
	terminal: ToolsServiceTerminalInfo;
	instanceId?: string;
}

let terminalContainer: HTMLElement | null = null;
let chatTerminalContainer: HTMLElement | null = null;

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

function TerminalOutput(props: TerminalViewProps): VNode {
	return (
		<TerminalAttachment
			connection={props.connection}
			className={props.compact ? "terminal-output chat-terminal-output" : "terminal-output"}
			ariaLabel={props.compact ? "Chat terminal output" : "Managed terminal output"}
			onConnectedChange={props.onConnectedChange}
			onController={props.onController}
			onStatus={props.onStatus}
		/>
	);
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
								title={`Terminal ${terminal.id} · ${state}`}
								aria-label={`Terminal ${terminal.id}, ${state}`}
								onClick={() => props.onSelectTerminal(terminal.id)}
							>
								<span>{terminal.id}</span>
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
				<TerminalOutput {...props} />
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
					onChange={(event) => props.onSelectSession(targetValue(event))}
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
							onClick={() => props.onSelectTerminal(terminal.id)}
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
				<TerminalOutput {...props} />
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
	};
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
	const controllerRef = useRef<TerminalAttachmentController | null>(null);

	function selectedSession(): TerminalSessionInfo | null {
		return terminalSessions(instances.value).find((session) => session.id === selectedSessionId.value) ?? null;
	}

	function terminalConnection(): TerminalConnection | null {
		const session = selectedSession();
		const terminal = session?.terminals.find((candidate) => candidate.id === selectedTerminalId.value);
		if (!(session && terminal)) return null;
		return {
			mode: "terminal",
			instanceId: session.instanceId,
			terminalId: terminal.id,
			sessionKey: terminal.sessionKey,
		};
	}

	function selectSession(sessionId: string): void {
		const session = terminalSessions(instances.value).find((candidate) => candidate.id === sessionId) ?? null;
		selectedSessionId.value = session?.id ?? "";
		selectedInstanceId.value = session?.instanceId ?? "";
		selectedTerminalId.value = session?.terminals[0]?.id ?? "";
	}

	function selectTerminal(terminalId: string): void {
		if (terminalId === selectedTerminalId.value && connected.value) {
			controllerRef.current?.focus();
			return;
		}
		selectedTerminalId.value = terminalId;
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
			selectedTerminalId.value = selection.terminalId;
			status.value = compact
				? ""
				: nextInstances.length === 0
					? "No active tools service instances are registered."
					: "Inventory refreshed.";
			statusLevel.value = compact ? "" : nextInstances.length === 0 ? "error" : "ok";
		} catch (error) {
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
		} catch (error) {
			status.value = error instanceof Error ? error.message : "Failed to create terminal";
			statusLevel.value = "error";
		} finally {
			creating.value = false;
		}
	}

	function control(action: "ctrl_c" | "clear" | "restart"): void {
		if (action === "restart") {
			controllerRef.current?.restart();
			return;
		}
		if (!controllerRef.current?.control(action)) {
			status.value = "Terminal is not connected.";
			statusLevel.value = "error";
		}
	}

	useEffect(() => {
		void refreshInventory();
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
			connection={terminalConnection()}
			onRefresh={refreshInventory}
			onCreate={createTerminal}
			onSelectSession={selectSession}
			onSelectTerminal={selectTerminal}
			onControl={control}
			onConnectedChange={(value) => {
				connected.value = value;
			}}
			onController={(controller) => {
				controllerRef.current = controller;
			}}
			onStatus={(text, level) => {
				status.value = text;
				statusLevel.value = level;
			}}
		/>
	);
}

export function initTerminal(container: HTMLElement): void {
	terminalContainer = container;
	container.classList.add("flex", "min-h-0", "flex-col", "overflow-hidden", "p-0");
	render(<TerminalPage />, container);
}

export function teardownTerminal(): void {
	if (terminalContainer) {
		render(null, terminalContainer);
		terminalContainer.classList.remove("flex", "min-h-0", "flex-col", "overflow-hidden", "p-0");
	}
	terminalContainer = null;
}

export function initChatTerminal(container: HTMLElement, sessionKey: string): void {
	chatTerminalContainer = container;
	render(<TerminalPage compact sessionKey={sessionKey} />, container);
}

export function teardownChatTerminal(): void {
	if (chatTerminalContainer) render(null, chatTerminalContainer);
	chatTerminalContainer = null;
}
