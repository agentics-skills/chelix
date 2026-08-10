// ── Session store (signal-based) ─────────────────────────────
//
// Single source of truth for session data. Each session becomes a
// Session class instance with per-session signals for client-side state.

import type { Signal } from "@preact/signals";
import { computed, signal } from "@preact/signals";
import type { ChannelBinding } from "../types/channel";
import type { SessionMeta, SessionTokens } from "../types/session";

// ── Session class ────────────────────────────────────────────

interface NormalizedSessionMeta {
	key: string;
	label: string;
	model: string;
	reasoningEffort: string;
	provider: string;
	projectId: string;
	messageCount: number;
	lastSeenMessageCount: number;
	preview: string;
	updatedAt: number;
	createdAt: number;
	worktreeBranch: string;
	channelBinding: ChannelBinding | null;
	parentSessionKey: string;
	forkPoint: number | null;
	agentId: string;
	externalAgentKind: string | null;
	externalSessionId: string | null;
	mcpDisabled: boolean | undefined;
	archived: boolean | undefined;
	activeChannel: string | undefined;
	version: number;
}

function stringValue(value: string | null | undefined, fallback = ""): string {
	return value || fallback;
}

function numberValue(value: number | undefined): number {
	return value || 0;
}

function nullableValue<T>(value: T | null | undefined): T | null {
	return value ?? null;
}

function firstNonEmptyString(values: Array<string | null | undefined>): string | null {
	return values.find(Boolean) || null;
}

function normalizeSessionMeta(serverData: SessionMeta, reasoningEffortFallback = ""): NormalizedSessionMeta {
	return {
		key: serverData.key,
		label: stringValue(serverData.label),
		model: stringValue(serverData.model),
		reasoningEffort: serverData.reasoningEffort ?? reasoningEffortFallback,
		provider: stringValue(serverData.provider),
		projectId: stringValue(serverData.projectId),
		messageCount: numberValue(serverData.messageCount),
		lastSeenMessageCount: numberValue(serverData.lastSeenMessageCount),
		preview: stringValue(serverData.preview),
		updatedAt: numberValue(serverData.updatedAt),
		createdAt: numberValue(serverData.createdAt),
		worktreeBranch: stringValue(serverData.worktree_branch),
		channelBinding: nullableValue(serverData.channelBinding),
		parentSessionKey: stringValue(serverData.parentSessionKey),
		forkPoint: nullableValue(serverData.forkPoint),
		agentId: stringValue(serverData.agent_id),
		externalAgentKind: firstNonEmptyString([serverData.external_agent_kind, serverData.externalAgentKind]),
		externalSessionId: firstNonEmptyString([serverData.externalSessionId]),
		mcpDisabled: serverData.mcpDisabled,
		archived: serverData.archived,
		activeChannel: serverData.activeChannel,
		version: numberValue(serverData.version),
	};
}

function nextSessionVersion(incoming: number, current: number): number {
	return incoming || current;
}

function isStaleSessionVersion(incoming: number, current: number): boolean {
	return incoming > 0 && current > 0 && incoming < current;
}

export class Session {
	// Server fields (plain properties, set on construction/update)
	key: string;
	label: string;
	model: string;
	reasoningEffort: string;
	provider: string;
	projectId: string;
	messageCount: number;
	lastSeenMessageCount: number;
	preview: string;
	updatedAt: number;
	createdAt: number;
	worktree_branch: string;
	channelBinding: ChannelBinding | null;
	parentSessionKey: string;
	forkPoint: number | null;
	agent_id: string;
	external_agent_kind: string | null;
	externalSessionId: string | null;
	mcpDisabled: boolean | undefined;
	archived: boolean | undefined;
	activeChannel: string | undefined;
	version: number;

	// Client signals (reactive, per-session)
	replying: Signal<boolean>;
	localUnread: Signal<boolean>;
	streamText: Signal<string>;
	voicePending: Signal<boolean>;
	activeRunId: Signal<string | null>;
	lastHistoryIndex: Signal<number>;
	sessionTokens: Signal<SessionTokens>;
	contextWindow: Signal<number>;
	toolsEnabled: Signal<boolean>;
	lastToolOutput: Signal<string>;
	badgeCount: Signal<number>;
	dataVersion: Signal<number>;

	constructor(serverData: SessionMeta) {
		const normalized = normalizeSessionMeta(serverData);
		// Server fields (plain properties, set on construction/update)
		this.key = normalized.key;
		this.label = normalized.label;
		this.model = normalized.model;
		this.reasoningEffort = normalized.reasoningEffort;
		this.provider = normalized.provider;
		this.projectId = normalized.projectId;
		this.messageCount = normalized.messageCount;
		this.lastSeenMessageCount = normalized.lastSeenMessageCount;
		this.preview = normalized.preview;
		this.updatedAt = normalized.updatedAt;
		this.createdAt = normalized.createdAt;
		this.worktree_branch = normalized.worktreeBranch;
		this.channelBinding = normalized.channelBinding;
		this.parentSessionKey = normalized.parentSessionKey;
		this.forkPoint = normalized.forkPoint;
		this.agent_id = normalized.agentId;
		this.external_agent_kind = normalized.externalAgentKind;
		this.externalSessionId = normalized.externalSessionId;
		this.mcpDisabled = normalized.mcpDisabled;
		this.archived = normalized.archived;
		this.activeChannel = normalized.activeChannel;
		this.version = normalized.version;

		// Client signals (reactive, per-session)
		this.replying = signal(false);
		this.localUnread = signal(false);
		this.streamText = signal("");
		this.voicePending = signal(false);
		this.activeRunId = signal<string | null>(null);
		this.lastHistoryIndex = signal(-1);
		this.sessionTokens = signal<SessionTokens>({ input: 0, output: 0 });
		this.contextWindow = signal(0);
		this.toolsEnabled = signal(true);
		this.lastToolOutput = signal("");
		// Total message count — reactive signal that drives the sidebar badge.
		// Components read this to show/hide badge and compute unread tinting.
		this.badgeCount = signal(this.messageCount);
		// Bumped whenever plain properties change so subscribed components re-render.
		this.dataVersion = signal(0);
	}

	/** Recalculate badge from current messageCount. */
	updateBadge(): void {
		this.badgeCount.value = this.messageCount;
	}

	/** Merge server fields, preserving client signals. Returns false if stale. */
	update(serverData: SessionMeta): boolean {
		const normalized = normalizeSessionMeta(serverData, this.reasoningEffort);
		if (isStaleSessionVersion(normalized.version, this.version)) return false;
		this.version = nextSessionVersion(normalized.version, this.version);
		this.label = normalized.label;
		this.model = normalized.model;
		this.reasoningEffort = normalized.reasoningEffort;
		this.provider = normalized.provider;
		this.projectId = normalized.projectId;
		// Only accept server counts when they've caught up with optimistic
		// client bumps. Authoritative resets (/clear, switchSession) use
		// syncCounts() which sets messageCount directly before any fetch.
		if (normalized.messageCount >= this.messageCount) {
			this.messageCount = normalized.messageCount;
			this.lastSeenMessageCount = normalized.lastSeenMessageCount;
			this.preview = normalized.preview;
			this.updatedAt = normalized.updatedAt;
		}
		this.createdAt = normalized.createdAt;
		this.worktree_branch = normalized.worktreeBranch;
		this.channelBinding = normalized.channelBinding;
		this.parentSessionKey = normalized.parentSessionKey;
		this.forkPoint = normalized.forkPoint;
		this.agent_id = normalized.agentId;
		this.external_agent_kind = normalized.externalAgentKind;
		this.externalSessionId = normalized.externalSessionId;
		this.mcpDisabled = normalized.mcpDisabled;
		this.archived = normalized.archived;
		this.activeChannel = normalized.activeChannel;
		this.updateBadge();
		this.dataVersion.value++;
		return true;
	}

	/** Optimistic bump: increment total and mark seen if active. */
	bumpCount(increment: number): void {
		this.messageCount = (this.messageCount || 0) + increment;
		if (this.key === activeSessionKey.value) {
			this.lastSeenMessageCount = this.messageCount;
		}
		this.updateBadge();
	}

	/** Authoritative set (switchSession history, /clear). */
	syncCounts(messageCount: number, lastSeenMessageCount: number): void {
		this.messageCount = messageCount;
		this.lastSeenMessageCount = lastSeenMessageCount;
		this.updateBadge();
	}

	/** Clear streaming state for this session. */
	resetStreamState(): void {
		this.streamText.value = "";
		this.voicePending.value = false;
		this.activeRunId.value = null;
		this.lastToolOutput.value = "";
	}

	/** Return a plain SessionMeta snapshot of this session's server fields. */
	toMeta(): SessionMeta {
		return {
			id: 0,
			key: this.key,
			label: this.label,
			model: this.model,
			reasoningEffort: this.reasoningEffort,
			provider: this.provider,
			createdAt: this.createdAt,
			updatedAt: this.updatedAt,
			messageCount: this.messageCount,
			lastSeenMessageCount: this.lastSeenMessageCount,
			projectId: this.projectId,
			worktree_branch: this.worktree_branch,
			channelBinding: this.channelBinding,
			activeChannel: this.activeChannel,
			parentSessionKey: this.parentSessionKey,
			forkPoint: this.forkPoint,
			mcpDisabled: this.mcpDisabled,
			preview: this.preview,
			archived: this.archived,
			agent_id: this.agent_id,
			external_agent_kind: this.external_agent_kind,
			externalSessionId: this.externalSessionId,
			version: this.version,
		};
	}
}

// ── Store signals ────────────────────────────────────────────
export const sessions = signal<Session[]>([]);
export const activeSessionKey = signal<string>(localStorage.getItem("chelix-session") || "main");
export const switchInProgress = signal<boolean>(false);
export const refreshInProgressKey = signal<string>("");
/** Session list tab filter: "all" | "sessions" | "cron" */
export const sessionListTab = signal<string>(localStorage.getItem("chelix-session-tab") || "sessions");
export const showArchivedSessions = signal<boolean>(localStorage.getItem("chelix-show-archived-sessions") === "1");

export const activeSession = computed<Session | null>(() => {
	const key = activeSessionKey.value;
	return sessions.value.find((s) => s.key === key) || null;
});

export function compareSessionOrder(left: Session | null, right: Session | null): number {
	const leftKey = left?.key || "";
	const rightKey = right?.key || "";
	const leftMain = leftKey === "main";
	const rightMain = rightKey === "main";
	if (leftMain !== rightMain) return leftMain ? -1 : 1;

	const updatedDiff = (Number(right?.updatedAt) || 0) - (Number(left?.updatedAt) || 0);
	if (updatedDiff !== 0) return updatedDiff;

	const createdDiff = (Number(right?.createdAt) || 0) - (Number(left?.createdAt) || 0);
	if (createdDiff !== 0) return createdDiff;

	return leftKey.localeCompare(rightKey);
}

export function insertSessionInOrder(list: Session[], session: Session): Session[] {
	if (!session?.key) return Array.isArray(list) ? list.slice() : [];
	const result = Array.isArray(list) ? list.filter((entry) => entry?.key !== session.key) : [];
	result.push(session);
	result.sort(compareSessionOrder);
	return result;
}

// ── Methods ──────────────────────────────────────────────────

/**
 * Replace the full sessions list from server data.
 * Reuses existing Session instances (matched by key) so their
 * client-side signals (replying, localUnread, streamText) are preserved.
 * New keys get fresh instances. Missing keys are dropped.
 */
function applyTransientSessionFlags(session: Session, data: SessionMeta): void {
	if (data._localUnread) session.localUnread.value = true;
	if (data._replying || data.replying) session.replying.value = true;
}

function mergeSessionData(existing: Map<string, Session>, data: SessionMeta): Session {
	const session = existing.get(data.key) || new Session(data);
	if (existing.has(data.key)) session.update(data);
	applyTransientSessionFlags(session, data);
	return session;
}

export function setAll(serverSessions: SessionMeta[]): void {
	const existing = new Map(sessions.value.map((session) => [session.key, session]));
	sessions.value = serverSessions.map((data) => mergeSessionData(existing, data));
}

/**
 * Upsert a single session from server data.
 * Reuses existing instance when present; creates and appends when missing.
 */
export function upsert(serverData: SessionMeta): Session | null {
	if (!serverData?.key) return null;
	const prev = getByKey(serverData.key);
	if (prev) {
		prev.update(serverData);
		sessions.value = insertSessionInOrder(sessions.value, prev);
		return prev;
	}
	const next = new Session(serverData);
	sessions.value = insertSessionInOrder(sessions.value, next);
	return next;
}

/** Remove a session by key. Returns true when a session was removed. */
export function remove(key: string): boolean {
	if (!key) return false;
	const existing = getByKey(key);
	if (!existing) return false;
	sessions.value = sessions.value.filter((session) => session.key !== key);
	if (activeSessionKey.value === key) {
		const fallback = sessions.value.find((session) => session.key === "main")?.key || sessions.value[0]?.key || "main";
		activeSessionKey.value = fallback;
		localStorage.setItem("chelix-session", fallback);
	}
	return true;
}

/** Fetch sessions from the server via HTTP (gzip-friendly). */
export function fetch(): Promise<void> {
	return window
		.fetch("/api/sessions", {
			headers: { Accept: "application/json" },
		})
		.then((response) => (response.ok ? response.json() : null))
		.then((payload: SessionMeta[] | null) => {
			if (!Array.isArray(payload)) return;
			setAll(payload);
		})
		.catch((error: unknown) => {
			console.warn("Failed to refresh sessions:", error);
		});
}

/** Notify Preact that session data changed (triggers re-render). */
export function notify(): void {
	sessions.value = [...sessions.value];
}

/** Look up a session by key. */
export function getByKey(key: string): Session | null {
	return sessions.value.find((s) => s.key === key) || null;
}

/** Set the active session key. Persists to localStorage. */
export function setActive(key: string): void {
	activeSessionKey.value = key;
	localStorage.setItem("chelix-session", key);
}

/** Set the session list tab and persist it. */
export function setSessionListTab(tab: string): void {
	sessionListTab.value = tab;
	localStorage.setItem("chelix-session-tab", tab);
}

/** Toggle whether archived sessions are shown in the sidebar. */
export function setShowArchivedSessions(show: boolean): void {
	showArchivedSessions.value = !!show;
	localStorage.setItem("chelix-show-archived-sessions", show ? "1" : "0");
}

export const sessionStore = {
	sessions,
	activeSessionKey,
	activeSession,
	switchInProgress,
	refreshInProgressKey,
	sessionListTab,
	showArchivedSessions,
	Session,
	setAll,
	upsert,
	remove,
	fetch,
	getByKey,
	setActive,
	setSessionListTab,
	setShowArchivedSessions,
	notify,
};
