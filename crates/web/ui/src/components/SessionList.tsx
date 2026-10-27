// ── SessionList Preact component ─────���───────────────────────
//
// Replaces the imperative renderSessionList() with a reactive Preact
// component that auto-rerenders from sessionStore signals.

import { Fragment, type VNode } from "preact";
import { useEffect, useLayoutEffect, useRef, useState } from "preact/hooks";
import { makeArchiveIcon, makeChatIcon, makeCronIcon, makeProjectIcon, makeTelegramIcon } from "../icons";
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
	return new Date(epochMs).toLocaleTimeString(undefined, {
		hour: "2-digit",
		minute: "2-digit",
		hourCycle: "h23",
	});
}

// ── Icon component (renders SVG icon into a ref) ────────────

const channelIconFactories: Partial<Record<ChannelType, () => HTMLSpanElement>> = {
	[ChannelType.Telegram]: makeTelegramIcon,
};

const channelLabels: Partial<Record<ChannelType, string>> = {
	[ChannelType.Telegram]: "Telegram",
};

function makePrimarySessionIcon(session: Session, channelType: ChannelType | null): HTMLElement {
	if ((session.key || "").startsWith("cron:")) return makeCronIcon();
	return channelType ? (channelIconFactories[channelType]?.() ?? makeChatIcon()) : makeChatIcon();
}

function syncSessionIcon(
	container: HTMLSpanElement,
	session: Session,
	channelType: ChannelType | null,
	archived: boolean,
): void {
	container.replaceChildren(makePrimarySessionIcon(session, channelType));
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
		<span
			className="session-badge inline-block align-top mt-[calc((18px-0.7rem)/2)] h-[0.7rem] text-[8.5px] leading-[0.7rem]"
			data-session-key={sessionKey}
		>
			{count > 99 ? "99+" : String(count)}
		</span>
	);
}

interface SessionIconProps {
	session: Session;
}

function SessionIcon({ session }: SessionIconProps): VNode {
	const iconRef = useRef<HTMLSpanElement>(null);
	const archived = Boolean(session.archived);
	const channelType = channelSessionType(session);

	useEffect(() => {
		const container = iconRef.current;
		if (!container) return;
		syncSessionIcon(container, session, channelType, archived);
	}, [session, channelType, archived]);

	const presentation = sessionIconPresentation(session, channelType, archived);
	return (
		<span className="session-icon mt-px" style={presentation.style} title={presentation.title}>
			<span className="session-icon-image" ref={iconRef} />
			<span className="session-spinner" />
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

function appendProjectMeta(container: HTMLSpanElement, project: ProjectInfo, followsText: boolean): void {
	if (followsText) container.appendChild(document.createTextNode(" \u00b7 "));
	const icon = makeProjectIcon();
	icon.style.display = "inline";
	icon.style.verticalAlign = "-1px";
	icon.style.marginRight = "2px";
	icon.style.opacity = "0.7";
	container.appendChild(icon);
	container.appendChild(document.createTextNode((project.label as string) || project.id));
}

function syncSessionMeta(container: HTMLSpanElement, session: Session): void {
	container.textContent = "";
	container.title = "";
	const parts = sessionMetaParts(session);
	const project = session.projectId ? projectStore.getById(session.projectId) : null;
	if (parts.length === 0 && !project) return;
	container.textContent = parts.join(" \u00b7 ");
	if (project) appendProjectMeta(container, project, parts.length > 0);
	container.title = container.textContent;
}

function SessionMeta({ session }: SessionMetaProps): VNode {
	const ref = useRef<HTMLSpanElement>(null);
	const dataVersion = session.dataVersion.value;

	useEffect(() => {
		const container = ref.current;
		if (!container) return;
		syncSessionMeta(container, session);
	}, [session, dataVersion]);

	return <span className="session-meta" data-session-key={session.key} ref={ref} />;
}

// ── SessionItem component ───────────���───────────────────────

interface KeyMap {
	[key: string]: Session;
}

interface SessionAncestor {
	key: string;
	continues: boolean;
}

interface SessionItemProps {
	session: Session;
	activeKey: string;
	ancestors: SessionAncestor[];
	hasChildren: boolean;
	keyMap: KeyMap;
	refreshing: boolean;
}

interface SessionTreeGuidesProps {
	ancestors: SessionAncestor[];
}

function SessionTreeGuides({ ancestors }: SessionTreeGuidesProps): VNode {
	return (
		<span className="session-tree-guides" aria-hidden="true">
			{ancestors.map((ancestor, index) => {
				const classes = ["session-tree-guide"];
				if (ancestor.continues) classes.push("continues");
				if (index === ancestors.length - 1) classes.push("branch");
				return <span key={ancestor.key} className={classes.join(" ")} />;
			})}
		</span>
	);
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
		<span className="session-agent float-left mr-1.5" title={`Agent: ${agentId}`}>
			@{agentId}
		</span>
	);
}

interface SessionTimeProps {
	timestamp?: number;
	hasBadge: boolean;
}

function SessionTime({ timestamp = 0, hasBadge }: SessionTimeProps): VNode | null {
	if (timestamp <= 0) return null;
	return (
		<span
			className={`session-time${hasBadge ? " ml-0.5" : ""}`}
			title={new Date(timestamp).toLocaleString(undefined, { hourCycle: "h23" })}
		>
			{formatHHMM(timestamp)}
		</span>
	);
}

interface SessionLabelProps {
	session: Session;
	badge: number;
}

function SessionLabel({ session, badge }: SessionLabelProps): VNode {
	void session.dataVersion.value;
	const label = session.label || session.key;
	const titleRef = useRef<HTMLDivElement>(null);
	const measureRef = useRef<HTMLSpanElement>(null);
	const metadataRef = useRef<HTMLSpanElement>(null);
	const [singleLine, setSingleLine] = useState(false);

	useLayoutEffect(() => {
		const title = titleRef.current;
		const measure = measureRef.current;
		const metadata = metadataRef.current;
		if (!(title && measure && metadata)) return;

		const update = () => {
			const textWidth = measure.getBoundingClientRect().width;
			const metadataWidth = metadata.getBoundingClientRect().width;
			const marginLeft = Number.parseFloat(getComputedStyle(metadata).marginLeft);
			setSingleLine(textWidth + metadataWidth + marginLeft <= title.getBoundingClientRect().width);
		};
		const observer = new ResizeObserver(update);
		observer.observe(title);
		observer.observe(measure);
		observer.observe(metadata);
		update();
		return () => observer.disconnect();
	}, [label, badge, session.updatedAt, session.archived]);

	return (
		<div className="session-label flex items-start gap-0.5">
			<SessionIcon session={session} />
			<div className={`min-w-0 flex-1 line-clamp-2 break-all ${singleLine ? "h-[18px]" : "h-[36px]"}`} ref={titleRef}>
				<span>
					<span className={`float-right w-0 ${singleLine ? "h-0" : "h-[18px]"}`} aria-hidden="true" />
					<span className="float-right clear-right ml-0.5 h-[18px] whitespace-nowrap" ref={metadataRef}>
						<SessionBadge count={badge} sessionKey={session.key} />
						<SessionTime timestamp={session.updatedAt} hasBadge={badge > 0} />
					</span>
					<span data-label-text title={label}>
						{label}
					</span>
				</span>
			</div>
			<div className="absolute invisible h-0 w-0 overflow-hidden" aria-hidden="true">
				<span className="inline-block whitespace-nowrap" data-label-text ref={measureRef}>
					{label}
				</span>
			</div>
		</div>
	);
}

interface SessionPreviewProps {
	preview: string;
}

function SessionPreview({ preview }: SessionPreviewProps): VNode | null {
	return preview ? (
		<span className="session-preview" title={preview}>
			{preview}
		</span>
	) : null;
}

function SessionItem({ session, activeKey, ancestors, hasChildren, keyMap, refreshing }: SessionItemProps): VNode {
	const groupEnd = !hasChildren && ancestors.every((ancestor) => !ancestor.continues);
	const active = session.key === activeKey;
	// Read per-session signals — auto-subscribes for re-render.
	// dataVersion triggers re-render when plain properties (preview,
	// updatedAt, label) change. Badge updates come from badgeCount.
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
	const href = sessionPath(session.key);

	return (
		<a
			href={href}
			className={`${className}${groupEnd ? " session-group-end" : ""}`}
			data-session-key={session.key}
			onClick={(event) => navigateToSession(event, href, session.key)}
		>
			<SessionTreeGuides ancestors={ancestors} />
			<div className={`session-info${hasChildren ? " session-parent" : ""}`}>
				<SessionLabel session={session} badge={badge} />
				<div className="session-details">
					<div className="session-summary line-clamp-2 break-all">
						<span>
							<SessionAgentBadge agentId={session.agent_id} />
							<SessionPreview preview={sessionPreview(session, keyMap)} />
							<SessionMeta session={session} />
						</span>
					</div>
				</div>
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

	function renderTree(session: Session, ancestors: SessionAncestor[]): VNode {
		const children = childrenMap[session.key] || [];
		return (
			<Fragment key={session.key}>
				<SessionItem
					session={session}
					activeKey={activeKey}
					ancestors={ancestors}
					hasChildren={children.length > 0}
					keyMap={keyMap}
					refreshing={session.key === refreshingKey}
				/>
				{children.map((child, index) =>
					renderTree(child, [...ancestors, { key: session.key, continues: index < children.length - 1 }]),
				)}
			</Fragment>
		);
	}

	return <div ref={spinnersRef}>{roots.map((s) => renderTree(s, []))}</div>;
}
