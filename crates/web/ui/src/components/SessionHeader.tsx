// ── SessionHeader Preact component ───────────────────────────
//
// Replaces the imperative updateChatSessionHeader() with a reactive
// Preact component reading sessionStore.activeSession.

import type { RefObject, VNode } from "preact";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import * as gon from "../gon";
import { parseAgentsListPayload, sendRpc } from "../helpers";
import {
	clearActiveSession,
	clearSessionHistoryCache,
	fetchSessions,
	isArchivableSession,
	removeSessionFromClientState,
	setSessionActiveRunId,
	setSessionAgent,
	setSessionReplying,
	switchSession,
} from "../sessions";
import { sessionStore } from "../stores/session-store";
import type { RpcResponse } from "../types/rpc";
import { ComboSelect, confirmDialog, shareLinkDialog, shareVisibilityDialog, showToast } from "../ui";

// ── Types ────────────────────────────────────────────────────

interface AgentOption {
	id: string;
	name: string;
	emoji?: string;
	is_default?: boolean;
	[key: string]: unknown;
}

interface ExternalAgentInfo {
	kind: string;
	name: string;
	installed: boolean;
	version?: string | null;
}

interface SelectOption {
	value: string;
	label: string;
}

interface SharePayload {
	path?: string;
	accessKey?: string;
}

export interface SessionHeaderProps {
	showSelectors?: boolean;
	showName?: boolean;
	showShare?: boolean;
	showFork?: boolean;
	showStop?: boolean;
	showClear?: boolean;
	showDelete?: boolean;
	showArchive?: boolean;
	nameOwnLine?: boolean;
	showRenameButton?: boolean;
	actionButtonClass?: string;
	onBeforeShare?: (() => void) | null;
	onBeforeArchive?: (() => void) | null;
	onBeforeDelete?: (() => void) | null;
}

// ── Helpers ──────────────────────────────────────────────────

function nextSessionKey(currentKey: string): string {
	const allSessions = sessionStore.sessions.value;
	const s = allSessions.find((x) => x.key === currentKey);
	if (s?.parentSessionKey) return s.parentSessionKey;
	const idx = allSessions.findIndex((x) => x.key === currentKey);
	if (idx >= 0 && idx + 1 < allSessions.length) return allSessions[idx + 1].key;
	if (idx > 0) return allSessions[idx - 1].key;
	return "main";
}

function buildShareUrl(payload: SharePayload): string {
	let url = `${window.location.origin}${payload.path}`;
	if (payload.accessKey) {
		url += `?k=${encodeURIComponent(payload.accessKey)}`;
	}
	return url;
}

async function copyShareUrl(url: string, visibility: string): Promise<void> {
	try {
		if (navigator.clipboard?.writeText) {
			await navigator.clipboard.writeText(url);
			showToast("Share link copied", "success");
			return;
		}
	} catch (_err) {
		// Clipboard APIs can fail on some browsers/permissions.
	}
	await shareLinkDialog(url, visibility);
}

interface SessionDeleteContext {
	currentKey: string;
	nextKey: string;
	canOptimisticallyDelete: boolean;
}

function sessionDeleteError(response: RpcResponse | undefined): string {
	const error = response?.error as unknown;
	if (typeof error === "string") return error;
	if (error && typeof error === "object" && "message" in error && typeof error.message === "string") {
		return error.message;
	}
	return "";
}

function hasUncommittedChanges(error: string): boolean {
	return error.includes("uncommitted changes");
}

function applyDeletedSessionState(context: SessionDeleteContext): void {
	removeSessionFromClientState(context.currentKey, { nextKey: context.nextKey });
	switchSession(context.nextKey);
}

async function deleteSession(context: SessionDeleteContext, force = false): Promise<void> {
	const optimisticallyApplied = context.canOptimisticallyDelete && !force;
	if (optimisticallyApplied) applyDeletedSessionState(context);
	const response = await sendRpc(
		"sessions.delete",
		force ? { key: context.currentKey, force: true } : { key: context.currentKey },
	);
	const error = sessionDeleteError(response);
	if (!response?.ok && hasUncommittedChanges(error)) {
		fetchSessions();
		if (await confirmDialog("Worktree has uncommitted changes. Force delete?")) await deleteSession(context, true);
		return;
	}
	if (!response?.ok) {
		showToast(error || "Failed to delete session", "error");
		fetchSessions();
		return;
	}
	if (!optimisticallyApplied) applyDeletedSessionState(context);
	fetchSessions();
}

function shouldConfirmSessionDelete(messageCount: number, forkPoint: number | null | undefined): boolean {
	const isUnmodifiedFork = forkPoint != null && messageCount <= forkPoint;
	return messageCount > 0 && !isUnmodifiedFork;
}

async function confirmAndDeleteSession(context: SessionDeleteContext, needsConfirmation: boolean): Promise<void> {
	if (needsConfirmation && !(await confirmDialog("Delete this session?"))) return;
	await deleteSession(context);
}

function initialAgentOptions(payload: ReturnType<typeof parseAgentsListPayload>): AgentOption[] {
	return Array.isArray(payload?.agents) ? (payload.agents as AgentOption[]) : [];
}

function initialDefaultAgentId(payload: ReturnType<typeof parseAgentsListPayload>): string {
	return typeof payload?.defaultId === "string" ? payload.defaultId : "main";
}

function currentExternalAgent(agents: ExternalAgentInfo[], kind: string): ExternalAgentInfo | null {
	return kind ? agents.find((agent) => agent.kind === kind) || null : null;
}

function sessionDisplayName(
	label: string | null | undefined,
	sessionKey: string | undefined,
	currentKey: string,
	nameOwnLine: boolean,
): string {
	const fullName = label || sessionKey || currentKey;
	return nameOwnLine || fullName.length <= 20 ? fullName : `${fullName.slice(0, 20)}\u2026`;
}

function agentSelectOptions(
	agents: AgentOption[],
	currentAgentId: string,
	defaultAgentId: string,
	switchingAgent: boolean,
	agentOptionsLoaded: boolean,
): SelectOption[] {
	const options = agents.map((agent) => ({
		value: agent.id,
		label: `${agent.emoji ? `${agent.emoji} ` : ""}${agent.name}${agent.id === defaultAgentId ? " (default)" : ""}`,
	}));
	const hasCurrentAgent = agents.some((agent) => agent.id === currentAgentId);
	if (hasCurrentAgent || !currentAgentId || !(switchingAgent || agentOptionsLoaded)) return options;
	return [{ value: currentAgentId, label: switchingAgent ? "Switching\u2026" : `agent:${currentAgentId}` }, ...options];
}

function externalAgentSelectOptions(agents: ExternalAgentInfo[]): SelectOption[] {
	return [
		{ value: "", label: "Chelix agent" },
		...agents.map((agent) => ({
			value: agent.kind,
			label: `${agent.name}${agent.installed ? "" : " (unavailable)"}`,
		})),
	];
}

function externalAgentStatus(
	kind: string,
	agent: ExternalAgentInfo | null,
	externalSessionId: string | null | undefined,
): string {
	if (!kind) return "";
	if (agent?.installed === false) return "External agent unavailable";
	return externalSessionId ? `External session ${externalSessionId}` : "External agent bound";
}

function sessionNameStyle(canRename: boolean, nameOwnLine: boolean): Record<string, string> {
	const style: Record<string, string> = { cursor: canRename ? "pointer" : "default" };
	if (nameOwnLine) {
		style.color = "var(--text-strong)";
		style.wordBreak = "break-word";
	}
	return style;
}

interface SessionNameControlProps {
	show: boolean;
	renaming: boolean;
	inputRef: RefObject<HTMLInputElement>;
	renameInputStyle: Record<string, string> | undefined;
	nameStyle: Record<string, string>;
	canRename: boolean;
	displayName: string;
	onCommit: () => void;
	onKeyDown: (event: KeyboardEvent) => void;
	onStart: () => void;
}

function SessionNameControl(props: SessionNameControlProps): VNode | null {
	if (!props.show) return null;
	if (props.renaming) {
		return (
			<input
				ref={props.inputRef}
				className="chat-session-rename-input"
				style={props.renameInputStyle}
				onBlur={props.onCommit}
				onKeyDown={props.onKeyDown}
			/>
		);
	}
	return (
		<button
			type="button"
			className="chat-session-name"
			style={props.nameStyle}
			title={props.canRename ? "Click to rename" : ""}
			disabled={!props.canRename}
			onClick={props.onStart}
		>
			{props.displayName}
		</button>
	);
}

interface RenameActionsProps {
	showName: boolean;
	showRenameButton: boolean;
	canRename: boolean;
	renaming: boolean;
	actionButtonClass: string;
	generatingTitle: boolean;
	onRename: () => void;
	onGenerateTitle: () => void;
}

function RenameActions(props: RenameActionsProps): VNode | null {
	if (!(props.showName && props.showRenameButton && props.canRename && !props.renaming)) return null;
	return (
		<div className="flex items-center gap-1">
			<button type="button" className={props.actionButtonClass} onClick={props.onRename} title="Rename session">
				Rename
			</button>
			<button
				type="button"
				className={props.actionButtonClass}
				onClick={props.onGenerateTitle}
				disabled={props.generatingTitle}
				title="Auto-generate title from conversation"
			>
				{props.generatingTitle ? "..." : "Auto-title"}
			</button>
		</div>
	);
}

interface AgentPickerProps {
	show: boolean;
	options: SelectOption[];
	value: string;
	disabled: boolean;
	onChange: (value: string) => void;
}

function AgentPicker(props: AgentPickerProps): VNode | null {
	if (!props.show) return null;
	return (
		<ComboSelect
			options={props.options}
			value={props.value}
			onChange={props.onChange}
			placeholder="Session agent"
			searchable={false}
			allowEmpty={false}
			fullWidth={false}
			disabled={props.disabled}
			floating
		/>
	);
}

interface ExternalAgentPickerProps {
	show: boolean;
	options: SelectOption[];
	value: string;
	status: string;
	disabled: boolean;
	onChange: (value: string) => void;
}

function ExternalAgentPicker(props: ExternalAgentPickerProps): VNode | null {
	if (!props.show) return null;
	return (
		<div className="flex items-center gap-1.5" data-testid="external-agent-picker">
			<ComboSelect
				options={props.options}
				value={props.value}
				onChange={props.onChange}
				placeholder="External agent"
				searchable={false}
				allowEmpty={false}
				fullWidth={false}
				disabled={props.disabled}
				floating
			/>
			{props.status && (
				<span className="text-xs text-[var(--text-muted)]" title={props.status}>
					{props.status}
				</span>
			)}
		</div>
	);
}

interface SessionSelectorsProps {
	showSelectors: boolean;
	isCron: boolean;
	agentOptionsLoaded: boolean;
	agents: AgentOption[];
	currentAgentId: string;
	defaultAgentId: string;
	switchingAgent: boolean;
	onAgentChange: (value: string) => void;
	externalAgents: ExternalAgentInfo[];
	currentExternalAgentKind: string;
	currentExternalAgent: ExternalAgentInfo | null;
	externalSessionId: string | null | undefined;
	switchingExternalAgent: boolean;
	onExternalAgentChange: (value: string) => void;
}

function SessionSelectors(props: SessionSelectorsProps): VNode {
	const hasCurrentAgent = props.agents.some((agent) => agent.id === props.currentAgentId);
	const options = agentSelectOptions(
		props.agents,
		props.currentAgentId,
		props.defaultAgentId,
		props.switchingAgent,
		props.agentOptionsLoaded,
	);
	return (
		<>
			<AgentPicker
				show={
					props.showSelectors &&
					!props.isCron &&
					props.agentOptionsLoaded &&
					(props.agents.length > 1 || !hasCurrentAgent)
				}
				options={options}
				value={props.currentAgentId}
				disabled={props.switchingAgent || options.length === 0}
				onChange={props.onAgentChange}
			/>
			<ExternalAgentPicker
				show={props.showSelectors && !props.isCron && props.externalAgents.length > 0}
				options={externalAgentSelectOptions(props.externalAgents)}
				value={props.currentExternalAgentKind}
				status={externalAgentStatus(
					props.currentExternalAgentKind,
					props.currentExternalAgent,
					props.externalSessionId,
				)}
				disabled={props.switchingExternalAgent}
				onChange={props.onExternalAgentChange}
			/>
		</>
	);
}

interface HeaderActionProps {
	show: boolean;
	className: string;
	onClick: () => void;
}

function ArchiveAction(props: HeaderActionProps & { archived: boolean }): VNode | null {
	if (!props.show) return null;
	return (
		<button
			type="button"
			className={props.className}
			onClick={props.onClick}
			title={props.archived ? "Unarchive session" : "Archive session"}
		>
			{props.archived ? "Unarchive" : "Archive"}
		</button>
	);
}

function IconAction(props: HeaderActionProps & { icon: string; title: string; label: string }): VNode | null {
	if (!props.show) return null;
	return (
		<button
			type="button"
			className={`${props.className} inline-flex items-center gap-1.5`}
			onClick={props.onClick}
			title={props.title}
		>
			<span className={`icon icon-sm ${props.icon} shrink-0`} />
			{props.label}
		</button>
	);
}

function DeleteAction(props: HeaderActionProps): VNode | null {
	if (!props.show) return null;
	return (
		<button
			type="button"
			className={`${props.className} chat-session-btn-danger inline-flex items-center gap-1.5`}
			onClick={props.onClick}
			title="Delete session"
			style={{ background: "var(--error)", borderColor: "var(--error)", color: "#fff" }}
		>
			<span className="icon icon-sm icon-x-circle shrink-0" />
			Delete
		</button>
	);
}

function PendingAction(
	props: HeaderActionProps & { title: string; pending: boolean; idleLabel: string; pendingLabel: string },
): VNode | null {
	if (!props.show) return null;
	return (
		<button
			type="button"
			className={props.className}
			onClick={props.onClick}
			title={props.title}
			disabled={props.pending}
		>
			{props.pending ? props.pendingLabel : props.idleLabel}
		</button>
	);
}

interface SessionActionsProps {
	actionButtonClass: string;
	showArchive: boolean;
	canArchive: boolean;
	archived: boolean;
	onArchive: () => void;
	showFork: boolean;
	showShare: boolean;
	isCron: boolean;
	onFork: () => void;
	onShare: () => void;
	showDelete: boolean;
	isMain: boolean;
	onDelete: () => void;
	showStop: boolean;
	canStop: boolean;
	stopping: boolean;
	onStop: () => void;
	showClear: boolean;
	clearing: boolean;
	onClear: () => void;
}

function SessionActions(props: SessionActionsProps): VNode {
	return (
		<>
			<ArchiveAction
				show={props.showArchive && props.canArchive}
				className={props.actionButtonClass}
				onClick={props.onArchive}
				archived={props.archived}
			/>
			<IconAction
				show={props.showFork && !props.isCron}
				className={props.actionButtonClass}
				onClick={props.onFork}
				icon="icon-layers"
				title="Fork session"
				label="Fork"
			/>
			<IconAction
				show={props.showShare && !props.isCron}
				className={props.actionButtonClass}
				onClick={props.onShare}
				icon="icon-share"
				title="Share snapshot"
				label="Share"
			/>
			<DeleteAction
				show={props.showDelete && !props.isMain}
				className={props.actionButtonClass}
				onClick={props.onDelete}
			/>
			<PendingAction
				show={props.showStop && props.canStop}
				className={props.actionButtonClass}
				onClick={props.onStop}
				title="Stop generation"
				pending={props.stopping}
				idleLabel="Stop"
				pendingLabel="Stopping\u2026"
			/>
			<PendingAction
				show={props.showClear && props.isMain}
				className={props.actionButtonClass}
				onClick={props.onClear}
				title="Clear session"
				pending={props.clearing}
				idleLabel="Clear"
				pendingLabel="Clearing\u2026"
			/>
		</>
	);
}

interface SessionHeaderViewProps {
	nameOwnLine: boolean;
	showName: boolean;
	nameControl: VNode | null;
	renameActions: VNode | null;
	agentPicker: VNode | null;
	externalAgentPicker: VNode | null;
	actions: VNode;
}

function SessionHeaderView(props: SessionHeaderViewProps): VNode {
	const controls = (
		<>
			{props.agentPicker}
			{props.externalAgentPicker}
			{!props.nameOwnLine && props.nameControl}
			{!props.nameOwnLine && props.renameActions}
			{props.actions}
		</>
	);
	if (!props.nameOwnLine) return <div className="flex items-center gap-2">{controls}</div>;
	return (
		<div className="flex flex-col gap-2 w-full">
			{props.showName && (
				<div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-2 w-full">
					<div className="min-w-0">{props.nameControl}</div>
					<div className="justify-self-end">{props.renameActions}</div>
				</div>
			)}
			<div className="flex flex-wrap items-center gap-2">{controls}</div>
		</div>
	);
}

// ── Component ────────────────────────────────────────────────

export function SessionHeader({
	showSelectors = true,
	showName = true,
	showShare = true,
	showFork = true,
	showStop = true,
	showClear = true,
	showDelete = true,
	showArchive = true,
	nameOwnLine = false,
	showRenameButton = false,
	actionButtonClass = "chat-session-btn",
	onBeforeShare = null,
	onBeforeArchive = null,
	onBeforeDelete = null,
}: SessionHeaderProps = {}): VNode {
	const session = sessionStore.activeSession.value;
	const sessionDataVersion = session?.dataVersion.value || 0;
	const currentKey = sessionStore.activeSessionKey.value;
	const gonAgentsPayload = parseAgentsListPayload(gon.get("agents") as never);
	const hydratedAgentOptions = initialAgentOptions(gonAgentsPayload);
	const hydratedDefaultAgentId = initialDefaultAgentId(gonAgentsPayload);

	const [renaming, setRenaming] = useState(false);
	const [clearing, setClearing] = useState(false);
	const [stopping, setStopping] = useState(false);
	const [switchingAgent, setSwitchingAgent] = useState(false);
	const [agentOptions, setAgentOptions] = useState<AgentOption[]>(hydratedAgentOptions);
	const [defaultAgentId, setDefaultAgentId] = useState(hydratedDefaultAgentId);
	const [agentOptionsLoaded, setAgentOptionsLoaded] = useState(hydratedAgentOptions.length > 0);
	const [externalAgentOptions, setExternalAgentOptions] = useState<ExternalAgentInfo[]>([]);
	const [switchingExternalAgent, setSwitchingExternalAgent] = useState(false);
	const inputRef = useRef<HTMLInputElement>(null);

	const fullName = session?.label || session?.key || currentKey;
	const displayName = sessionDisplayName(session?.label, session?.key, currentKey, nameOwnLine);
	const replying = session?.replying.value;
	const activeRunId = session?.activeRunId.value || null;

	const isMain = currentKey === "main";
	const isCron = currentKey.startsWith("cron:");
	const canRename = !(isMain || isCron);
	const canStop = !isCron && replying;
	const canArchive = !!session && isArchivableSession(session.toMeta());
	const showArchivedSessions = sessionStore.showArchivedSessions.value;
	const currentAgentId = session?.agent_id || defaultAgentId || "main";
	const currentExternalAgentKind = session?.external_agent_kind || "";
	const selectedExternalAgent = currentExternalAgent(externalAgentOptions, currentExternalAgentKind);

	useEffect(() => {
		let cancelled = false;
		sendRpc("agents.list", {}).then((res) => {
			if (cancelled) return;
			if (!res?.ok) {
				setAgentOptionsLoaded(true);
				return;
			}
			const parsed = parseAgentsListPayload(res.payload as never);
			setDefaultAgentId(parsed.defaultId);
			setAgentOptions(parsed.agents as AgentOption[]);
			setAgentOptionsLoaded(true);
		});
		return () => {
			cancelled = true;
		};
	}, [currentKey]);

	useEffect(() => {
		let cancelled = false;
		sendRpc<ExternalAgentInfo[]>("external_agents.list", {}).then((res) => {
			if (cancelled || !res?.ok) return;
			setExternalAgentOptions(Array.isArray(res.payload) ? res.payload : []);
		});
		return () => {
			cancelled = true;
		};
	}, [currentKey]);

	const startRename = useCallback(() => {
		if (!canRename) return;
		setRenaming(true);
	}, [canRename]);

	// Populate, focus, and select the rename input synchronously after
	// render (useLayoutEffect) so there is no rAF race with Playwright
	// or other async interactions that could blur the input.
	useLayoutEffect(() => {
		if (renaming && inputRef.current) {
			inputRef.current.value = fullName;
			inputRef.current.focus();
			inputRef.current.select();
		}
	}, [renaming, fullName]);

	const commitRename = useCallback(() => {
		if (!inputRef.current) return;
		const val = inputRef.current.value.trim() || "";
		setRenaming(false);
		if (val && val !== fullName) {
			sendRpc("sessions.patch", { key: currentKey, label: val }).then((res) => {
				if (res?.ok) fetchSessions();
			});
		}
	}, [currentKey, fullName]);

	const onKeyDown = useCallback(
		(e: KeyboardEvent) => {
			if (e.key === "Enter" && !e.isComposing) {
				e.preventDefault();
				commitRename();
			}
			if (e.key === "Escape") {
				setRenaming(false);
			}
		},
		[commitRename],
	);

	const [generatingTitle, setGeneratingTitle] = useState(false);
	const onGenerateTitle = useCallback(() => {
		setGeneratingTitle(true);
		sendRpc<{ label?: string }>("sessions.generate_title", { key: currentKey })
			.then((res) => {
				if (!res?.ok) {
					showToast(res?.error?.message || "Failed to auto-generate title", "error");
					return;
				}
				fetchSessions();
			})
			.finally(() => setGeneratingTitle(false));
	}, [currentKey]);

	const onFork = useCallback(() => {
		sendRpc<{ sessionKey?: string }>("sessions.fork", { key: currentKey }).then((res) => {
			if (res?.ok && res.payload?.sessionKey) {
				fetchSessions();
				switchSession(res.payload.sessionKey);
			}
		});
	}, [currentKey]);

	const onDelete = useCallback(() => {
		onBeforeDelete?.();
		const currentSession = sessionStore.getByKey(currentKey);
		const messageCount = currentSession?.messageCount || 0;
		const context: SessionDeleteContext = {
			currentKey,
			nextKey: nextSessionKey(currentKey),
			canOptimisticallyDelete: !currentSession?.worktree_branch,
		};
		void confirmAndDeleteSession(context, shouldConfirmSessionDelete(messageCount, currentSession?.forkPoint));
	}, [currentKey, onBeforeDelete, sessionDataVersion]);

	const onClear = useCallback(() => {
		if (clearing) return;
		setClearing(true);
		clearActiveSession().finally(() => {
			setClearing(false);
		});
	}, [clearing]);

	const onStop = useCallback(() => {
		if (stopping) return;
		const params: Record<string, unknown> = { sessionKey: currentKey };
		if (activeRunId) params.runId = activeRunId;
		setStopping(true);
		sendRpc("chat.abort", params)
			.then((res) => {
				if (!res?.ok) {
					showToast((res?.error as { message?: string })?.message || "Failed to stop response", "error");
					return;
				}
				setSessionActiveRunId(currentKey, null);
				setSessionReplying(currentKey, false);
			})
			.finally(() => {
				setStopping(false);
			});
	}, [activeRunId, currentKey, stopping]);

	const shareSnapshot = useCallback(
		async (visibility: string) => {
			const res = await sendRpc<SharePayload>("sessions.share.create", { key: currentKey, visibility });
			if (!(res?.ok && res.payload?.path)) {
				showToast((res?.error as { message?: string })?.message || "Failed to create share link", "error");
				return;
			}

			const url = buildShareUrl(res.payload);
			clearSessionHistoryCache(currentKey);
			switchSession(currentKey);
			fetchSessions();

			await copyShareUrl(url, visibility);

			if (visibility === "private") {
				showToast("Private link includes a key, share it only with trusted people", "success");
			}
		},
		[currentKey],
	);

	const onShare = useCallback(() => {
		if (typeof onBeforeShare === "function") {
			onBeforeShare();
		}
		shareVisibilityDialog().then((visibility) => {
			if (!visibility) return;
			void shareSnapshot(visibility);
		});
	}, [onBeforeShare, shareSnapshot]);

	const onArchive = useCallback(() => {
		if (!(session && canArchive)) return;
		if (typeof onBeforeArchive === "function") {
			onBeforeArchive();
		}
		const nextArchived = !session.archived;
		sendRpc("sessions.patch", { key: currentKey, archived: nextArchived }).then((res) => {
			if (!res?.ok) {
				showToast((res?.error as { message?: string })?.message || "Failed to update archive state", "error");
				return;
			}
			if (session) {
				session.archived = nextArchived;
				session.dataVersion.value++;
			}
			if (nextArchived && !showArchivedSessions) {
				switchSession("main");
			}
			fetchSessions();
		});
	}, [canArchive, currentKey, onBeforeArchive, session, showArchivedSessions]);

	const onAgentChange = useCallback(
		(nextAgentId: string) => {
			if (!nextAgentId || nextAgentId === currentAgentId || switchingAgent) {
				return;
			}
			setSwitchingAgent(true);
			setSessionAgent(currentKey, nextAgentId)
				.then((res) => {
					if (!res?.ok) {
						showToast((res?.error as { message?: string })?.message || "Failed to switch agent", "error");
						return;
					}
					fetchSessions();
				})
				.finally(() => {
					setSwitchingAgent(false);
				});
		},
		[currentAgentId, currentKey, switchingAgent],
	);

	const onExternalAgentChange = useCallback(
		(nextKind: string) => {
			if (switchingExternalAgent || nextKind === currentExternalAgentKind) return;
			setSwitchingExternalAgent(true);
			const request = nextKind
				? sendRpc("external_agents.bind", { sessionKey: currentKey, kind: nextKind })
				: sendRpc("external_agents.unbind", { sessionKey: currentKey });
			request
				.then((res) => {
					if (!res?.ok) {
						showToast((res?.error as { message?: string })?.message || "Failed to update external agent", "error");
						return;
					}
					if (session) {
						session.external_agent_kind = nextKind || null;
						session.dataVersion.value++;
					}
					fetchSessions();
				})
				.finally(() => {
					setSwitchingExternalAgent(false);
				});
		},
		[currentExternalAgentKind, currentKey, session, switchingExternalAgent],
	);

	const nameControl = (
		<SessionNameControl
			show={showName}
			renaming={renaming}
			inputRef={inputRef}
			renameInputStyle={nameOwnLine ? { maxWidth: "none", width: "100%" } : undefined}
			nameStyle={sessionNameStyle(canRename, nameOwnLine)}
			canRename={canRename}
			displayName={displayName}
			onCommit={commitRename}
			onKeyDown={onKeyDown}
			onStart={startRename}
		/>
	);
	const renameActions = (
		<RenameActions
			showName={showName}
			showRenameButton={showRenameButton}
			canRename={canRename}
			renaming={renaming}
			actionButtonClass={actionButtonClass}
			generatingTitle={generatingTitle}
			onRename={startRename}
			onGenerateTitle={onGenerateTitle}
		/>
	);
	const selectors = (
		<SessionSelectors
			showSelectors={showSelectors}
			isCron={isCron}
			agentOptionsLoaded={agentOptionsLoaded}
			agents={agentOptions}
			currentAgentId={currentAgentId}
			defaultAgentId={defaultAgentId}
			switchingAgent={switchingAgent}
			onAgentChange={onAgentChange}
			externalAgents={externalAgentOptions}
			currentExternalAgentKind={currentExternalAgentKind}
			currentExternalAgent={selectedExternalAgent}
			externalSessionId={session?.externalSessionId}
			switchingExternalAgent={switchingExternalAgent}
			onExternalAgentChange={onExternalAgentChange}
		/>
	);
	const actions = (
		<SessionActions
			actionButtonClass={actionButtonClass}
			showArchive={showArchive}
			canArchive={canArchive}
			archived={!!session?.archived}
			onArchive={onArchive}
			showFork={showFork}
			showShare={showShare}
			isCron={isCron}
			onFork={onFork}
			onShare={onShare}
			showDelete={showDelete}
			isMain={isMain}
			onDelete={onDelete}
			showStop={showStop}
			canStop={!!canStop}
			stopping={stopping}
			onStop={onStop}
			showClear={showClear}
			clearing={clearing}
			onClear={onClear}
		/>
	);

	return (
		<SessionHeaderView
			nameOwnLine={nameOwnLine}
			showName={showName}
			nameControl={nameControl}
			renameActions={renameActions}
			agentPicker={selectors}
			externalAgentPicker={null}
			actions={actions}
		/>
	);
}
