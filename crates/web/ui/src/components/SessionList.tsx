// ── SessionList Preact component ─────���───────────────────────
//
// Replaces the imperative renderSessionList() with a reactive Preact
// component that auto-rerenders from sessionStore signals.

import type { VNode } from "preact";
import { useEffect, useRef } from "preact/hooks";
import {
	makeArchiveIcon,
	makeBranchIcon,
	makeChatIcon,
	makeCronIcon,
	makeDiscordIcon,
	makeMatrixIcon,
	makeProjectIcon,
	makeSlackIcon,
	makeTelegramIcon,
} from "../icons";
import { currentPrefix, navigate, sessionPath } from "../router";
import { switchSession } from "../sessions";
import * as projectStore from "../stores/project-store";
import { type Session, sessionStore } from "../stores/session-store";
import { ChannelType } from "../types/channel";
import type { ProjectInfo } from "../types/project";

// ── Braille spinner ───────────��─────────────────────────────
const spinnerFrames: string[] = [
	"\u280B",
	"\u2819",
	"\u2839",
	"\u2838",
	"\u283C",
	"\u2834",
	"\u2826",
	"\u2827",
	"\u2807",
	"\u280F",
];

// ── Helpers ─────────���────────────────────────────────────────

function channelSessionType(s: Session): ChannelType | null {
	const key = s.key || "";
	if (key.startsWith(`${ChannelType.Telegram}:`)) return ChannelType.Telegram;
	if (key.startsWith(`${ChannelType.Discord}:`)) return ChannelType.Discord;
	if (key.startsWith(`${ChannelType.Slack}:`)) return ChannelType.Slack;
	if (key.startsWith(`${ChannelType.Matrix}:`)) return ChannelType.Matrix;
	const binding = s.channelBinding || null;
	if (!binding) return null;
	try {
		const parsed = typeof binding === "string" ? JSON.parse(binding) : binding;
		return (parsed.channel_type as ChannelType) || null;
	} catch (_e) {
		return null;
	}
}

function formatHHMM(epochMs: number): string {
	return new Date(epochMs).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
}

// ── Icon component (renders SVG icon into a ref) ────────────

const channelIconFactories: Partial<Record<ChannelType, () => HTMLSpanElement>> = {
	[ChannelType.Telegram]: makeTelegramIcon,
	[ChannelType.Discord]: makeDiscordIcon,
	[ChannelType.Slack]: makeSlackIcon,
	[ChannelType.Matrix]: makeMatrixIcon,
};

const channelLabels: Partial<Record<ChannelType, string>> = {
	[ChannelType.Discord]: "Discord",
	[ChannelType.Slack]: "Slack",
	[ChannelType.Matrix]: "Matrix",
	[ChannelType.Telegram]: "Telegram",
};

function makePrimarySessionIcon(session: Session, isBranch: boolean, channelType: ChannelType | null): HTMLElement {
	if (isBranch) return makeBranchIcon();
	if ((session.key || "").startsWith("cron:")) return makeCronIcon();
	return channelType ? (channelIconFactories[channelType]?.() ?? makeChatIcon()) : makeChatIcon();
}

function syncSessionIcon(
	container: HTMLSpanElement,
	session: Session,
	isBranch: boolean,
	channelType: ChannelType | null,
	archived: boolean,
): void {
	container.replaceChildren(makePrimarySessionIcon(session, isBranch, channelType));
	if (!archived) return;
	const mark = makeArchiveIcon();
	mark.classList.add("session-archived-mark");
	container.appendChild(mark);
}

interface SessionIconPresentation {
	style: Record<string, string>;
	title: string;
}

function sessionIconPresentation(
	session: Session,
	channelType: ChannelType | null,
	archived: boolean,
): SessionIconPresentation {
	if (!channelType) {
		return {
			style: { color: "var(--muted)" },
			title: archived ? "Archived session" : "",
		};
	}

	const active = Boolean(session.activeChannel);
	const channelLabel = channelLabels[channelType] ?? "Telegram";
	const baseTitle = active ? `Active ${channelLabel} session` : `${channelLabel} session (inactive)`;
	return {
		style: {
			color: active ? "var(--accent)" : "var(--muted)",
			opacity: active ? "1" : "0.5",
		},
		title: archived ? `${baseTitle} \u00b7 Archived` : baseTitle,
	};
}

interface SessionBadgeProps {
	count: number;
	sessionKey: string;
}

function SessionBadge({ count, sessionKey }: SessionBadgeProps): VNode | null {
	if (count <= 0) return null;
	return (
		<span className="session-badge" data-session-key={sessionKey}>
			{count > 99 ? "99+" : String(count)}
		</span>
	);
}

interface SessionIconProps {
	session: Session;
	isBranch: boolean;
}

function SessionIcon({ session, isBranch }: SessionIconProps): VNode {
	const iconRef = useRef<HTMLSpanElement>(null);
	const archived = Boolean(session.archived);
	const channelType = channelSessionType(session);

	useEffect(() => {
		const container = iconRef.current;
		if (!container) return;
		syncSessionIcon(container, session, isBranch, channelType, archived);
	}, [session, isBranch, channelType, archived]);

	const presentation = sessionIconPresentation(session, channelType, archived);
	// Read the reactive signal — auto-subscribes for badge updates.
	const count = session.badgeCount.value;

	return (
		<span className="session-icon" style={presentation.style} title={presentation.title}>
			<span ref={iconRef} />
			<span className="session-spinner" />
			<SessionBadge count={count} sessionKey={session.key} />
		</span>
	);
}

// ── Session meta (fork, worktree, project) ──────────────────

interface SessionMetaProps {
	session: Session;
}

function sessionMetaParts(session: Session): string[] {
	const parts: string[] = [];
	if (session.forkPoint != null) parts.push(`fork@${session.forkPoint}`);
	if (session.worktree_branch) parts.push(`\u2387 ${session.worktree_branch}`);
	return parts;
}

function appendProjectMeta(container: HTMLDivElement, project: ProjectInfo, followsText: boolean): void {
	if (followsText) container.appendChild(document.createTextNode(" \u00b7 "));
	const icon = makeProjectIcon();
	icon.style.display = "inline";
	icon.style.verticalAlign = "-1px";
	icon.style.marginRight = "2px";
	icon.style.opacity = "0.7";
	container.appendChild(icon);
	container.appendChild(document.createTextNode((project.label as string) || project.id));
}

function syncSessionMeta(container: HTMLDivElement, session: Session): void {
	container.textContent = "";
	const parts = sessionMetaParts(session);
	const project = session.projectId ? projectStore.getById(session.projectId) : null;
	if (parts.length === 0 && !project) return;
	container.textContent = parts.join(" \u00b7 ");
	if (project) appendProjectMeta(container, project, parts.length > 0);
}

function SessionMeta({ session }: SessionMetaProps): VNode {
	const ref = useRef<HTMLDivElement>(null);
	const dataVersion = session.dataVersion.value;

	useEffect(() => {
		const container = ref.current;
		if (!container) return;
		syncSessionMeta(container, session);
	}, [session, dataVersion]);

	return <div className="session-meta" data-session-key={session.key} ref={ref} />;
}

// ── SessionItem component ───────────���───────────────────────

interface KeyMap {
	[key: string]: Session;
}

interface SessionItemProps {
	session: Session;
	activeKey: string;
	depth: number;
	keyMap: KeyMap;
	refreshing: boolean;
}

interface SessionItemState {
	active: boolean;
	unread: boolean;
	replying: boolean;
	refreshing: boolean;
	archived: boolean;
}

function sessionItemClassName(state: SessionItemState): string {
	const classes = ["session-item"];
	if (state.active) classes.push("active");
	if (state.unread) classes.push("unread");
	if (state.replying) classes.push("replying");
	if (state.refreshing) classes.push("loading");
	if (state.archived) classes.push("archived");
	return classes.join(" ");
}

function sessionHasUnread(session: Session, active: boolean, badge: number): boolean {
	return session.localUnread.value || (!active && badge > (session.lastSeenMessageCount || 0));
}

function sessionPreview(session: Session, keyMap: KeyMap): string {
	const preview = session.preview || "";
	const parentPreview = keyMap[session.parentSessionKey || ""]?.preview || "";
	return preview && preview === parentPreview ? "" : preview;
}

function navigateToSession(event: MouseEvent, href: string, sessionKey: string): void {
	if (event.defaultPrevented) return;
	if (event.button !== 0) return;
	if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
	event.preventDefault();
	if (currentPrefix !== "/chats") {
		navigate(href);
		return;
	}
	switchSession(sessionKey);
}

interface SessionAgentBadgeProps {
	agentId?: string;
}

function SessionAgentBadge({ agentId = "main" }: SessionAgentBadgeProps): VNode | null {
	if (!agentId || agentId === "main") return null;
	return (
		<span
			className="text-[10px] text-[var(--muted)] border border-[var(--border)] rounded px-1 py-0 ml-1"
			title={`Agent: ${agentId}`}
		>
			@{agentId}
		</span>
	);
}

interface SessionTimeProps {
	timestamp?: number;
}

function SessionTime({ timestamp = 0 }: SessionTimeProps): VNode | null {
	if (timestamp <= 0) return null;
	return (
		<span className="session-time" title={new Date(timestamp).toLocaleString()}>
			{formatHHMM(timestamp)}
		</span>
	);
}

interface SessionPreviewProps {
	preview: string;
}

function SessionPreview({ preview }: SessionPreviewProps): VNode | null {
	return preview ? <div className="session-preview">{preview}</div> : null;
}

function SessionItem({ session, activeKey, depth, keyMap, refreshing }: SessionItemProps): VNode {
	const isBranch = depth > 0;
	const active = session.key === activeKey;
	// Read per-session signals — auto-subscribes for re-render.
	// dataVersion triggers re-render when plain properties (preview,
	// updatedAt, label) change. Badge updates come from badgeCount
	// signal read inside SessionIcon.
	const replying = session.replying.value;
	void session.dataVersion.value;
	// Unread tint: true when not viewing this session and there are messages
	// beyond what we last saw (badgeCount is reactive, triggers re-render).
	const badge = session.badgeCount.value;
	const unread = sessionHasUnread(session, active, badge);
	const className = sessionItemClassName({
		active,
		unread,
		replying,
		refreshing,
		archived: Boolean(session.archived),
	});
	const style = isBranch ? { paddingLeft: `${12 + depth * 16}px` } : {};
	const href = sessionPath(session.key);

	return (
		<a
			href={href}
			className={className}
			data-session-key={session.key}
			style={style}
			onClick={(event) => navigateToSession(event, href, session.key)}
		>
			<div className="session-info">
				<div className="session-label">
					<SessionIcon session={session} isBranch={isBranch} />
					<span data-label-text>{session.label || session.key}</span>
					<SessionAgentBadge agentId={session.agent_id} />
					<SessionTime timestamp={session.updatedAt} />
				</div>
				<SessionPreview preview={sessionPreview(session, keyMap)} />
				<SessionMeta session={session} />
			</div>
		</a>
	);
}

// ── SessionList component ──────────────────���────────────────
export function SessionList(): VNode {
	const allSessions = sessionStore.sessions.value;
	const activeKey = sessionStore.activeSessionKey.value;
	const refreshingKey = sessionStore.refreshInProgressKey.value;
	const filterId = projectStore.projectFilterId.value;
	const tab = sessionStore.sessionListTab.value;
	const showArchived = sessionStore.showArchivedSessions.value;

	// Spinner animation via setInterval
	const spinnersRef = useRef<HTMLDivElement>(null);
	useEffect(() => {
		let idx = 0;
		const timer = setInterval(() => {
			idx = (idx + 1) % spinnerFrames.length;
			if (!spinnersRef.current) return;
			const els = spinnersRef.current.querySelectorAll(
				".session-item.replying .session-spinner, .session-item.loading .session-spinner",
			);
			for (const el of els) el.textContent = spinnerFrames[idx];
		}, 80);
		return () => clearInterval(timer);
	}, []);

	let filtered = filterId ? allSessions.filter((s) => s.projectId === filterId) : allSessions;
	if (tab === "sessions") {
		filtered = filtered.filter((s) => !(s.key || "").startsWith("cron:") && (showArchived || !s.archived));
	} else if (tab === "cron") {
		filtered = filtered.filter((s) => (s.key || "").startsWith("cron:"));
	}

	// Build parent→children map for tree rendering
	const childrenMap: Record<string, Session[]> = {};
	const keyMap: KeyMap = {};
	filtered.forEach((s) => {
		keyMap[s.key] = s;
		if (s.parentSessionKey) {
			if (!childrenMap[s.parentSessionKey]) childrenMap[s.parentSessionKey] = [];
			childrenMap[s.parentSessionKey].push(s);
		}
	});
	const roots = filtered.filter((s) => !(s.parentSessionKey && keyMap[s.parentSessionKey]));

	function renderTree(session: Session, depth: number): VNode {
		const children = childrenMap[session.key] || [];
		return (
			<>
				<SessionItem
					key={session.key}
					session={session}
					activeKey={activeKey}
					depth={depth}
					keyMap={keyMap}
					refreshing={session.key === refreshingKey}
				/>
				{children.map((child) => renderTree(child, depth + 1))}
			</>
		);
	}

	return <div ref={spinnersRef}>{roots.map((s) => renderTree(s, 0))}</div>;
}
