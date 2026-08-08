// ── Hooks page ─────────────────────────────────────────────

import { signal, useSignal } from "@preact/signals";
import type { RefObject, VNode } from "preact";
import { render } from "preact";
import { useEffect, useRef } from "preact/hooks";
import { Loading } from "../components/forms/ListItem";
import { onEvent } from "../events";
import { sendRpc } from "../helpers";
import { updateNavCount } from "../nav-counts";
import { copyToClipboard } from "../ui";

// ── Types ───────────────────────────────────────────────────

interface Hook {
	name: string;
	description?: string;
	emoji?: string;
	enabled: boolean;
	eligible: boolean;
	source: string;
	source_path: string;
	events: string[];
	command?: string;
	priority: number;
	timeout: number;
	body: string;
	body_html?: string;
	call_count: number;
	failure_count: number;
	avg_latency_ms: number;
	missing_os: boolean;
	missing_bins: string[];
	missing_env: string[];
}

interface ToastEntry {
	id: number;
	message: string;
	type: string;
}

// ── Signals ─────────────────────────────────────────────────
const hooks = signal<Hook[]>([]);
const loading = signal(false);
const toasts = signal<ToastEntry[]>([]);
let toastId = 0;

// ── Helpers ─────────────────────────────────────────────────
function showToast(message: string, type: string): void {
	const id = ++toastId;
	toasts.value = toasts.value.concat([{ id, message, type }]);
	setTimeout(() => {
		toasts.value = toasts.value.filter((entry) => entry.id !== id);
	}, 4000);
}

async function refreshHooks(): Promise<void> {
	loading.value = true;
	try {
		const res = await fetch("/api/hooks");
		if (res.ok) {
			const data = await res.json();
			hooks.value = data?.hooks || [];
		}
	} catch {
		const rpc = await sendRpc("hooks.list", {});
		if (rpc.ok) hooks.value = (rpc.payload as { hooks?: Hook[] })?.hooks || [];
	}
	loading.value = false;
	updateNavCount("hooks", hooks.value.length);
}

// ── Components ──────────────────────────────────────────────

function Toasts(): VNode | null {
	if (toasts.value.length === 0) return null;
	return (
		<div className="fixed bottom-4 right-4 z-50 flex flex-col gap-2">
			{toasts.value.map((entry) => (
				<div
					key={entry.id}
					className={
						"px-4 py-2.5 rounded-[var(--radius)] text-sm shadow-lg border " +
						(entry.type === "error"
							? "bg-[var(--danger-bg)] border-[var(--danger)] text-[var(--danger)]"
							: "bg-[var(--surface2)] border-[var(--border)] text-[var(--text)]")
					}
				>
					{entry.message}
				</div>
			))}
		</div>
	);
}

function StatusBadge({ hook }: { hook: Hook }): VNode {
	if (!hook.eligible) {
		return <span className="tier-badge">Ineligible</span>;
	}
	if (!hook.enabled) {
		return <span className="tier-badge">Disabled</span>;
	}
	return <span className="recommended-badge">Active</span>;
}

function SourceBadge({ source }: { source: string }): VNode {
	const label =
		source === "project" ? "Project" : source === "user" ? "User" : source === "builtin" ? "Built-in" : source;
	return <span className="tier-badge">{label}</span>;
}

// Safe: body_html is server-rendered trusted HTML produced by the Rust gateway
// (pulldown-cmark), NOT user-supplied browser input. Same pattern as page-skills.js
// which also renders server-produced HTML.
function MarkdownPreview({ html: serverHtml }: { html: string }): VNode {
	const divRef = useRef<HTMLDivElement>(null);
	useEffect(() => {
		// Server-rendered trusted HTML from pulldown-cmark; safe to assign directly.
		if (divRef.current) divRef.current.innerHTML = serverHtml || "";
	}, [serverHtml]);
	return (
		<div
			ref={divRef}
			className="skill-body-md text-sm bg-[var(--surface2)] border border-[var(--border)] rounded-[var(--radius-sm)] p-3 overflow-y-auto"
			style={{ minHeight: "120px", maxHeight: "400px" }}
		/>
	);
}

function hookMissingInfo(hook: Hook): string[] {
	const info: string[] = [];
	if (hook.missing_os) info.push("OS not supported");
	if (hook.missing_bins.length) info.push(`Missing: ${hook.missing_bins.join(", ")}`);
	if (hook.missing_env.length) info.push(`Env: ${hook.missing_env.join(", ")}`);
	return info;
}

function HookStats({ hook }: { hook: Hook }): VNode | null {
	if (!(hook.enabled && hook.call_count > 0)) return null;
	return (
		<>
			<span title="Calls">{hook.call_count} calls</span>
			{hook.failure_count > 0 && <span className="text-[var(--danger)]">{hook.failure_count} failed</span>}
			{hook.avg_latency_ms > 0 && <span>{hook.avg_latency_ms}ms avg</span>}
		</>
	);
}

function HookCardHeader({ hook, expanded, onToggle }: { hook: Hook; expanded: boolean; onToggle: () => void }): VNode {
	return (
		<button
			type="button"
			className="flex w-full items-center gap-3 border-0 bg-transparent px-4 py-3 text-left font-[inherit] cursor-pointer hover:bg-[var(--bg-hover)]"
			onClick={onToggle}
		>
			{hook.emoji && <span className="text-base">{hook.emoji}</span>}
			<span className="flex-1 min-w-0">
				<span className="flex items-center gap-2">
					<span className="text-sm font-medium text-[var(--text-strong)]">{hook.name}</span>
					<StatusBadge hook={hook} />
					<SourceBadge source={hook.source} />
				</span>
				{hook.description && (
					<span className="block text-xs text-[var(--muted)] mt-0.5 truncate">{hook.description}</span>
				)}
			</span>
			<span className="flex items-center gap-2 text-xs text-[var(--muted)] shrink-0">
				<HookStats hook={hook} />
				<span className={`icon icon-chevron-down transition-transform ${expanded ? "rotate-180" : ""}`} />
			</span>
		</button>
	);
}

function HookMetadata({ hook, missingInfo }: { hook: Hook; missingInfo: string[] }): VNode {
	function copySourcePath(): void {
		copyToClipboard(hook.source_path, "", "").then((ok) =>
			showToast(ok ? "Path copied" : "Could not copy path", ok ? "success" : "error"),
		);
	}
	return (
		<>
			<div className="flex flex-wrap gap-2 text-xs">
				<span className="text-[var(--muted)]">Events:</span>
				{hook.events.map((event) => (
					<span key={event} className="tier-badge">
						{event}
					</span>
				))}
			</div>
			{hook.command && (
				<div className="flex items-center gap-2 text-xs">
					<span className="text-[var(--muted)]">Command:</span>
					<code className="font-mono text-[var(--text)]">{hook.command}</code>
				</div>
			)}
			<div className="flex items-center gap-2 text-xs text-[var(--muted)]">
				<span>Priority: {hook.priority}</span>
				<span>Timeout: {hook.timeout}s</span>
				<button
					type="button"
					className="truncate cursor-pointer border-0 bg-transparent p-0 text-[inherit] font-[inherit] hover:text-[var(--text)] transition-colors"
					title="Click to copy path"
					onClick={copySourcePath}
				>
					{hook.source_path}
				</button>
			</div>
			{missingInfo.length > 0 && (
				<div className="text-xs text-[var(--warn)] bg-[rgba(234,179,8,0.08)] border border-[var(--warn)] rounded-[var(--radius-sm)] px-3 py-2">
					{missingInfo.join(" \u2022 ")}
				</div>
			)}
		</>
	);
}

interface HookEditorProps {
	hook: Hook;
	tab: "preview" | "source";
	content: string;
	textareaRef: RefObject<HTMLTextAreaElement>;
	onTabChange: (tab: "preview" | "source") => void;
	onInput: (event: Event) => void;
}

function hookEditorTabClass(active: boolean): string {
	const base = "px-3 py-1.5 text-xs font-medium rounded-t-[var(--radius-sm)] transition-colors -mb-px ";
	return (
		base +
		(active
			? "bg-[var(--surface2)] border border-[var(--border)] border-b-[var(--surface2)] text-[var(--text-strong)]"
			: "text-[var(--muted)] hover:text-[var(--text)] hover:bg-[var(--bg-hover)] border border-transparent")
	);
}

function HookEditor(props: HookEditorProps): VNode {
	const builtIn = props.hook.source === "builtin";
	return (
		<div className="flex flex-col gap-1">
			{!builtIn && (
				<div className="flex items-center gap-1 border-b border-[var(--border)] px-1">
					<button
						type="button"
						className={hookEditorTabClass(props.tab === "preview")}
						onClick={() => props.onTabChange("preview")}
					>
						Preview
					</button>
					<button
						type="button"
						className={hookEditorTabClass(props.tab === "source")}
						onClick={() => props.onTabChange("source")}
					>
						Source
					</button>
				</div>
			)}
			{builtIn ? (
				<div
					className="skill-body-md text-sm bg-[var(--surface2)] border border-[var(--border)] rounded-[var(--radius-sm)] p-3 overflow-y-auto"
					style={{ minHeight: "60px", maxHeight: "400px" }}
				>
					<p>{props.hook.description}</p>
					<p className="mt-2">
						<a
							href={`https://github.com/agentics-skills/chelix/blob/main/${props.hook.source_path}`}
							target="_blank"
							rel="noopener noreferrer"
							className="text-[var(--accent)] hover:underline"
						>
							View source on GitHub {"\u2197"}
						</a>
					</p>
				</div>
			) : props.tab === "source" ? (
				<textarea
					ref={props.textareaRef}
					className="w-full font-mono text-xs bg-[var(--surface2)] border border-[var(--border)] rounded-[var(--radius-sm)] p-3 text-[var(--text)] focus:outline-none focus:border-[var(--border-strong)] resize-y"
					rows={16}
					spellcheck={false}
					value={props.content}
					onInput={props.onInput}
				/>
			) : (
				<MarkdownPreview html={props.hook.body_html || ""} />
			)}
		</div>
	);
}

interface HookActionsProps {
	hook: Hook;
	dirty: boolean;
	saving: boolean;
	onEnableDisable: () => void;
	onSave: () => void;
}

function HookActions(props: HookActionsProps): VNode {
	return (
		<div className="flex items-center gap-2">
			{props.hook.source !== "builtin" && (
				<button
					type="button"
					className={`provider-btn provider-btn-sm ${props.hook.enabled ? "provider-btn-secondary" : ""}`}
					onClick={props.onEnableDisable}
				>
					{props.hook.enabled ? "Disable" : "Enable"}
				</button>
			)}
			{props.dirty && (
				<button type="button" className="provider-btn provider-btn-sm" onClick={props.onSave} disabled={props.saving}>
					{props.saving ? "Saving\u2026" : "Save"}
				</button>
			)}
		</div>
	);
}

interface HookDetailsProps extends HookActionsProps, Omit<HookEditorProps, "hook"> {
	missingInfo: string[];
}

function HookDetails(props: HookDetailsProps): VNode {
	return (
		<div className="border-t border-[var(--border)] px-4 py-3 flex flex-col gap-3">
			<HookMetadata hook={props.hook} missingInfo={props.missingInfo} />
			<HookEditor
				hook={props.hook}
				tab={props.tab}
				content={props.content}
				textareaRef={props.textareaRef}
				onTabChange={props.onTabChange}
				onInput={props.onInput}
			/>
			<HookActions
				hook={props.hook}
				dirty={props.dirty}
				saving={props.saving}
				onEnableDisable={props.onEnableDisable}
				onSave={props.onSave}
			/>
		</div>
	);
}

function HookCard({ hook }: { hook: Hook }): VNode {
	const expanded = useSignal(false);
	const editContent = useSignal(hook.body);
	const saving = useSignal(false);
	const dirty = useSignal(false);
	const tab = useSignal<"preview" | "source">("preview");
	const textareaRef = useRef<HTMLTextAreaElement>(null);

	// Reset content when hook body changes (e.g. after reload).
	useEffect(() => {
		editContent.value = hook.body;
		dirty.value = false;
	}, [hook.body]);

	function handleToggle(): void {
		expanded.value = !expanded.value;
	}

	async function handleEnableDisable(): Promise<void> {
		const method = hook.enabled ? "hooks.disable" : "hooks.enable";
		const res = await sendRpc(method, { name: hook.name });
		if (res?.ok) {
			showToast(`Hook "${hook.name}" ${hook.enabled ? "disabled" : "enabled"}`, "success");
		} else {
			showToast(`Failed: ${res?.error?.message || "unknown error"}`, "error");
		}
	}

	async function handleSave(): Promise<void> {
		saving.value = true;
		const res = await sendRpc("hooks.save", {
			name: hook.name,
			content: editContent.value,
		});
		saving.value = false;
		if (res?.ok) {
			dirty.value = false;
			showToast(`Saved "${hook.name}"`, "success");
		} else {
			showToast(`Failed to save: ${res?.error?.message || "unknown error"}`, "error");
		}
	}

	function handleInput(e: Event): void {
		const target = e.target as HTMLTextAreaElement;
		editContent.value = target.value;
		dirty.value = target.value !== hook.body;
	}

	function handleTabChange(nextTab: "preview" | "source"): void {
		tab.value = nextTab;
	}

	return (
		<div className="bg-[var(--surface)] border border-[var(--border)] rounded-[var(--radius)] overflow-hidden">
			<HookCardHeader hook={hook} expanded={expanded.value} onToggle={handleToggle} />
			{expanded.value && (
				<HookDetails
					hook={hook}
					missingInfo={hookMissingInfo(hook)}
					tab={tab.value}
					content={editContent.value}
					textareaRef={textareaRef}
					onTabChange={handleTabChange}
					onInput={handleInput}
					dirty={dirty.value}
					saving={saving.value}
					onEnableDisable={handleEnableDisable}
					onSave={handleSave}
				/>
			)}
		</div>
	);
}

function HooksPageComponent(): VNode {
	useEffect(() => {
		refreshHooks();
		const off = onEvent("hooks.status", (payload: unknown) => {
			const data = payload as { hooks?: Hook[] };
			if (data?.hooks) {
				hooks.value = data.hooks;
				updateNavCount("hooks", data.hooks.length);
			} else {
				refreshHooks();
			}
		});
		return off;
	}, []);

	async function handleReload(): Promise<void> {
		loading.value = true;
		const res = await sendRpc("hooks.reload", {});
		if (res?.ok) {
			showToast("Hooks reloaded", "success");
		} else {
			showToast(`Reload failed: ${res?.error?.message || "unknown error"}`, "error");
		}
		loading.value = false;
	}

	return (
		<>
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<div className="flex items-center gap-3">
					<h2 className="text-lg font-medium text-[var(--text-strong)]">Hooks</h2>
					<button
						type="button"
						className="provider-btn provider-btn-secondary provider-btn-sm"
						onClick={handleReload}
						disabled={loading.value}
					>
						{loading.value ? "Reloading\u2026" : "Reload"}
					</button>
				</div>

				<div className="max-w-[600px] bg-[var(--surface2)] border border-[var(--border)] rounded-[var(--radius)] px-5 py-4 leading-relaxed">
					<p className="text-sm text-[var(--text)] mb-2.5">
						<strong className="text-[var(--text-strong)]">Hooks</strong> run shell commands in response to lifecycle
						events (tool calls, messages, sessions, etc.). They live in{" "}
						<code className="font-mono text-xs">.chelix/hooks/</code> directories.
					</p>
					<div className="flex items-center gap-2 my-3 px-3.5 py-2.5 bg-[var(--surface)] rounded-[var(--radius-sm)] font-mono text-xs text-[var(--text-strong)]">
						<span className="opacity-50">Event</span>
						<span className="text-[var(--accent)]">{"\u2192"}</span>
						<span>Hook Script</span>
						<span className="text-[var(--accent)]">{"\u2192"}</span>
						<span>Continue / Modify / Block</span>
					</div>
					<p className="text-xs text-[var(--muted)]">
						Each hook is a directory containing a <code className="font-mono">HOOK.md</code> file with TOML frontmatter
						(events, command, requirements) and optional documentation. Edit the content below and click{" "}
						<strong>Save</strong> to update.
					</p>
				</div>

				{hooks.value.length === 0 && !loading.value ? (
					<div className="max-w-[600px] text-sm text-[var(--muted)] px-1">
						No hooks discovered. Create a <code className="font-mono text-xs">HOOK.md</code> file in{" "}
						<code className="font-mono text-xs">.chelix/hooks/my-hook/</code> or{" "}
						<code className="font-mono text-xs">~/.chelix/hooks/my-hook/</code> to get started.
					</div>
				) : null}

				<div className="max-w-[900px] flex flex-col gap-3">
					{hooks.value.map((h) => (
						<HookCard key={h.name} hook={h} />
					))}
				</div>

				{loading.value && hooks.value.length === 0 ? (
					<Loading message="Loading hooks..." className="p-6 text-center text-[var(--muted)] text-sm" />
				) : null}
			</div>
			<Toasts />
		</>
	);
}

// ── Exported init/teardown for settings integration ─────────
let _hooksContainer: HTMLElement | null = null;

export function initHooks(container: HTMLElement): void {
	_hooksContainer = container;
	container.style.cssText = "flex-direction:column;padding:0;overflow:hidden;";
	render(<HooksPageComponent />, container);
}

export function teardownHooks(): void {
	if (_hooksContainer) render(null, _hooksContainer);
	_hooksContainer = null;
}
