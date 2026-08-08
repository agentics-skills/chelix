// ── Skills page (Preact + Signals) ───────────────────────────
// Note: body_html is server-rendered trusted content from SKILL.md
// processed by pulldown-cmark on the Rust gateway side.

import { computed, signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useRef } from "preact/hooks";
import { TabBar } from "../components/forms/Tabs";
import { onEvent } from "../events";
import { sendRpc } from "../helpers";
import { t } from "../i18n";
import { updateNavCount } from "../nav-counts";
import { registerPage } from "../router";
import { routes } from "../routes";
import * as S from "../state";
import {
	type BundledCategory,
	CATEGORY_META,
	categoryLabel,
	isDiscoveredSource,
	isRepoSource,
	SkillSource,
} from "../types/skill-source";
import { ConfirmDialog, requestConfirm } from "../ui";

// ── Types ────────────────────────────────────────────────────

interface SkillSummary {
	name: string;
	description?: string;
	category?: string;
	source?: string;
	enabled?: boolean;
	protected?: boolean;
	display_name?: string;
	quarantined?: boolean;
	trusted?: boolean;
	drifted?: boolean;
}
interface SkillDetail extends SkillSummary {
	body?: string;
	body_html?: string;
	author?: string;
	version?: string;
	homepage?: string;
	source_url?: string;
	commit_sha?: string;
	commit_url?: string;
	commit_age_days?: number;
	compatibility?: string;
	allowed_tools?: string[];
	license?: string;
	license_url?: string;
	provenance?: { original_source?: string; original_commit_sha?: string; imported_from?: string };
	quarantine_reason?: string;
}
interface RepoSummary {
	source: string;
	skill_count: number;
	enabled_count: number;
	trusted_count?: number;
	commit_sha?: string;
	quarantined?: boolean;
	drifted?: boolean;
	orphaned?: boolean;
	repo_name?: string;
	provenance?: { original_source?: string; original_commit_sha?: string; imported_from?: string };
	quarantine_reason?: string;
}
interface ToastItem {
	id: number;
	message: string;
	type: string;
}
interface InstallProgress {
	id: string;
	source: string;
	state: string;
}

const repos = signal<RepoSummary[]>([]);
const enabledSkills = signal<SkillSummary[]>([]);
const loading = signal(false);
const toasts = signal<ToastItem[]>([]);
let toastId = 0;
const installProgresses = signal<InstallProgress[]>([]);
let installProgressId = 0;

let prefetchPromise: Promise<unknown> | null = null;
function ensurePrefetch(): Promise<unknown> {
	if (!prefetchPromise)
		prefetchPromise = fetch("/api/skills")
			.then((r) => r.json())
			.then((data) => {
				if (data.skills) enabledSkills.value = data.skills;
				if (data.repos) repos.value = data.repos;
				return data;
			})
			.catch(() => null);
	return prefetchPromise;
}

const skillRepoMap = computed<Record<string, string>>(() => {
	const map: Record<string, string> = {};
	enabledSkills.value.forEach((s) => {
		if (s.source) map[s.name] = s.source;
	});
	return map;
});

function showToast(message: string, type: string): void {
	const id = ++toastId;
	toasts.value = toasts.value.concat([{ id, message, type }]);
	setTimeout(() => {
		toasts.value = toasts.value.filter((toast) => toast.id !== id);
	}, 4000);
}
function shortSha(sha: string | null | undefined): string {
	return sha?.slice(0, 12) || "";
}
function startInstallProgress(source: string, id?: string): string {
	const pid = id || `install-${++installProgressId}`;
	if (installProgresses.value.some((p) => p.id === pid)) return pid;
	installProgresses.value = [...installProgresses.value, { id: pid, source: source || "repository", state: "running" }];
	return pid;
}
function stopInstallProgress(id: string, _ok: boolean): void {
	installProgresses.value = installProgresses.value.filter((p) => p.id !== id);
}

function emergencyDisableAllSkills(): void {
	requestConfirm("Disable all third-party skills now?", { confirmLabel: "Disable All", danger: true }).then((yes) => {
		if (!yes) return;
		sendRpc("skills.emergency_disable", {}).then((res) => {
			if (!res?.ok) {
				showToast(`Emergency disable failed: ${res?.error?.message || "unknown"}`, "error");
				return;
			}
			showToast(`Disabled ${(res.payload as Record<string, number>)?.skills_disabled || 0} skills`, "success");
			fetchAll();
		});
	});
}

function fetchAll(): void {
	fetchAllAsync().catch(console.error);
}
function fetchAllAsync(): Promise<void> {
	loading.value = true;
	return fetch("/api/skills")
		.then((r) => r.json())
		.then((data) => {
			if (data.skills) enabledSkills.value = data.skills;
			if (data.repos) repos.value = data.repos;
			loading.value = false;
			updateNavCount("skills", (data.skills || []).length);
		})
		.catch(() => {
			loading.value = false;
		});
}

function doInstall(source: string): Promise<void> {
	if (!(source && S.connected)) {
		if (!S.connected) showToast("Not connected to gateway.", "error");
		return Promise.resolve();
	}
	const opId = `skills-install-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
	const pid = startInstallProgress(source, opId);
	return sendRpc("skills.install", { source, op_id: opId }).then((res) => {
		if (res?.ok) {
			const p = (res.payload || {}) as Record<string, unknown[]>;
			const installed = (p.installed || []) as Array<{ name?: string }>;
			showToast(`Installed ${source} (${installed.length} skills) — review and enable the skills you need.`, "success");
			fetchAll();
			stopInstallProgress(pid, true);
		} else {
			showToast(`Failed: ${res?.error?.message || "unknown error"}`, "error");
			stopInstallProgress(pid, false);
		}
	});
}
function doExportBundle(source: string, path: string | null): Promise<void> {
	if (!(source && S.connected)) return Promise.resolve();
	const params: Record<string, string> = { source };
	if (path) params.path = path;
	return sendRpc("skills.repos.export", params).then((res) => {
		if (res?.ok) showToast(`Exported ${source}`, "success");
		else showToast(`Failed: ${res?.error?.message || "unknown"}`, "error");
	});
}
export function doUnquarantine(source: string): Promise<void> {
	if (!(source && S.connected)) return Promise.resolve();
	return sendRpc("skills.repos.unquarantine", { source }).then((res) => {
		if (res?.ok) {
			showToast(`Cleared quarantine for ${source}`, "success");
			fetchAll();
		} else showToast(`Failed: ${res?.error?.message || "unknown"}`, "error");
	});
}
function searchSkills(source: string, query: string): Promise<SkillSummary[]> {
	return fetch(`/api/skills/search?source=${encodeURIComponent(source)}&q=${encodeURIComponent(query)}`)
		.then((r) => r.json())
		.then((d) => d.skills || []);
}

function InstallProgressBar(): VNode | null {
	const items = installProgresses.value;
	if (!items.length) return null;
	return (
		<div style={{ display: "flex", flexDirection: "column", gap: "8px" }}>
			{items.map((p) => (
				<div
					key={p.id}
					style={{
						border: "1px solid var(--border)",
						borderRadius: "var(--radius-sm)",
						padding: "8px 10px",
						background: "var(--surface)",
						fontSize: ".78rem",
						color: "var(--muted)",
					}}
				>
					<strong style={{ color: "var(--text-strong)" }}>Installing {p.source}...</strong>
					<div style={{ marginTop: "3px" }}>This may take a while.</div>
				</div>
			))}
		</div>
	);
}

function InstallBox(): VNode {
	const ref = useRef<HTMLInputElement>(null);
	const installing = useSignal(false);
	function go(): void {
		const v = ref.current?.value.trim();
		if (!v) return;
		installing.value = true;
		doInstall(v).then(() => {
			installing.value = false;
			if (ref.current) ref.current.value = "";
		});
	}
	return (
		<div className="skills-install-box">
			<input
				ref={ref}
				type="text"
				placeholder="owner/repo or full URL (e.g. anthropics/skills)"
				className="skills-install-input"
				onKeyDown={(e) => {
					if ((e as KeyboardEvent).key === "Enter") go();
				}}
			/>
			<button type="button" className="provider-btn" onClick={go} disabled={installing.value}>
				{installing.value ? "Installing\u2026" : "Install"}
			</button>
		</div>
	);
}
interface FeaturedSkill {
	repo: string;
	desc: string;
	hasRecipe?: boolean;
}
const featuredSkills: FeaturedSkill[] = [
	{ repo: "anthropics/skills", desc: "Official Anthropic agent skills" },
	{ repo: "vercel-labs/agent-skills", desc: "Vercel agent skills collection" },
	{ repo: "vercel-labs/skills", desc: "Vercel skills toolkit" },
	{
		repo: "garrytan/gbrain",
		desc: "Knowledge graph with hybrid search for agent memory",
		hasRecipe: true,
	},
];

/** After installing a repo with a recipe, fetch and display the post-install instructions. */
async function checkPostInstallRecipe(source: string): Promise<void> {
	const res = await sendRpc("skills.recipe", { source });
	if (!res?.ok) return;
	const payload = res.payload as Record<string, unknown> | undefined;
	if (!payload?.found) return;
	const recipe = payload.recipe as { title?: string; instructions?: string } | undefined;
	if (!recipe?.instructions) return;
	showToast(
		`${recipe.title || "Setup available"} \u2014 ask the agent: \u201Crun the ${source.split("/").pop() || source} setup recipe\u201D`,
		"success",
	);
}

/** Derive the GitHub avatar URL for an org/user from the repo identifier. */
/** GitHub avatar URL — github.com/{owner}.png redirects to the correct avatar
 *  for both users and organizations. CSP img-src allows both domains. */
function orgAvatarUrl(repo: string): string {
	const owner = repo.split("/")[0];
	return `https://github.com/${owner}.png?size=40`;
}

/** Build the correct external link for a repo source. */
function repoHref(source: string): string | null {
	if (/^https?:\/\//.test(source)) return source;
	return `https://github.com/${source}`;
}

function FeaturedCard({ skill: f }: { skill: FeaturedSkill }): VNode {
	const installing = useSignal(false);
	const href = /^https?:\/\//.test(f.repo) ? f.repo : `https://github.com/${f.repo}`;
	const isInstalled = repos.value.some((r) => r.source === f.repo);
	return (
		<div className="skills-featured-card">
			<img
				src={orgAvatarUrl(f.repo)}
				alt=""
				style={{
					width: "24px",
					height: "24px",
					borderRadius: "var(--radius-sm)",
					flexShrink: 0,
				}}
			/>
			<div style={{ flex: 1, minWidth: 0 }}>
				<a
					href={href}
					target="_blank"
					rel="noopener noreferrer"
					style={{
						fontFamily: "var(--font-mono)",
						fontSize: ".82rem",
						fontWeight: 500,
						color: "var(--text-strong)",
						textDecoration: "none",
					}}
				>
					{f.repo}
				</a>
				<div style={{ fontSize: ".75rem", color: "var(--muted)" }}>{f.desc}</div>
			</div>
			<button
				type="button"
				onClick={() => {
					if (isInstalled) return;
					installing.value = true;
					doInstall(f.repo)
						.then(() => {
							if (f.hasRecipe) checkPostInstallRecipe(f.repo).catch(console.error);
						})
						.catch((err) => console.error("install failed", err))
						.finally(() => {
							installing.value = false;
						});
				}}
				disabled={isInstalled || installing.value}
				style={{
					background: "var(--surface2)",
					border: "1px solid var(--border)",
					color: isInstalled ? "var(--success, #22c55e)" : "var(--text)",
					borderRadius: "var(--radius-sm)",
					fontSize: ".72rem",
					padding: "4px 10px",
					cursor: isInstalled ? "default" : "pointer",
					whiteSpace: "nowrap",
					opacity: isInstalled ? 0.8 : 1,
				}}
			>
				{isInstalled ? "Installed" : installing.value ? "Installing\u2026" : "Install"}
			</button>
		</div>
	);
}
function FeaturedSection(): VNode {
	return (
		<div className="skills-section">
			<h3 className="skills-section-title">Featured Repositories</h3>
			<div className="skills-featured-grid">
				{featuredSkills.map((f) => (
					<FeaturedCard key={f.repo} skill={f} />
				))}
			</div>
		</div>
	);
}

function SkillMetadata({ detail: d }: { detail: SkillDetail }): VNode | null {
	if (!(d.author || d.version || d.homepage || d.source_url || d.commit_sha)) return null;
	return (
		<div
			style={{
				display: "flex",
				alignItems: "center",
				gap: "12px",
				marginBottom: "8px",
				fontSize: ".75rem",
				color: "var(--muted)",
				flexWrap: "wrap",
			}}
		>
			{d.author && <span>Author: {d.author}</span>}
			{d.version && <span>v{d.version}</span>}
			{d.commit_sha && (
				<span>
					Commit: <code>{shortSha(d.commit_sha)}</code>
				</span>
			)}
			{d.homepage && (
				<a
					href={d.homepage}
					target="_blank"
					rel="noopener noreferrer"
					style={{ color: "var(--accent)", textDecoration: "none" }}
				>
					{d.homepage.replace(/^https?:\/\//, "")}
				</a>
			)}
		</div>
	);
}

function skillActionClass(detail: SkillDetail, discovered: boolean): string {
	if (discovered && detail.enabled) return "provider-btn provider-btn-sm provider-btn-danger";
	if (detail.enabled) return "provider-btn provider-btn-sm provider-btn-secondary";
	return "provider-btn provider-btn-sm";
}

function skillActionLabel(detail: SkillDetail, discovered: boolean, busy: boolean): string {
	if (busy) return "...";
	if (discovered && detail.enabled) return "Delete";
	return detail.enabled ? "Disable" : "Install";
}

function SkillDetailPanel({
	detail: d,
	repoSource,
	onClose,
	onReload,
}: {
	detail: SkillDetail;
	repoSource?: string;
	onClose: () => void;
	onReload?: () => void;
}): VNode | null {
	const actionBusy = useSignal(false);
	const bodyRef = useRef<HTMLDivElement>(null);
	useEffect(() => {
		if (bodyRef.current && d?.body_html) {
			bodyRef.current.textContent = "";
			// Safe: body_html is server-rendered trusted HTML from SKILL.md via pulldown-cmark
			const tpl = document.createElement("template");
			tpl.innerHTML = d.body_html;
			bodyRef.current.appendChild(tpl.content);
			bodyRef.current.querySelectorAll("a").forEach((a) => {
				a.setAttribute("target", "_blank");
				a.setAttribute("rel", "noopener");
			});
		}
	}, [d?.body_html]);
	if (!d) return null;
	const isDisc = isDiscoveredSource(d.source);
	function doToggle(): void {
		actionBusy.value = true;
		sendRpc(d.enabled ? "skills.skill.disable" : "skills.skill.enable", { source: repoSource, skill: d.name }).then(
			(r) => {
				actionBusy.value = false;
				if (r?.ok) {
					if (isDisc) onClose();
					fetchAll();
					onReload?.();
				} else showToast(`Failed: ${r?.error?.message || "unknown"}`, "error");
			},
		);
	}
	function onToggle(): void {
		if (!S.connected || actionBusy.value) return;
		if (isDisc && d.protected) {
			showToast(`Protected`, "error");
			return;
		}
		if (isDisc && d.enabled) {
			requestConfirm(`Delete "${d.name}"?`, { confirmLabel: "Delete", danger: true }).then((y) => {
				if (y) doToggle();
			});
			return;
		}
		doToggle();
	}
	return (
		<div className="skills-detail-panel" style={{ display: "block" }}>
			<div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", marginBottom: "8px" }}>
				<span
					style={{ fontFamily: "var(--font-mono)", fontSize: ".9rem", fontWeight: 600, color: "var(--text-strong)" }}
				>
					{d.display_name || d.name}
				</span>
				<div style={{ display: "flex", gap: "6px" }}>
					<button type="button" onClick={onToggle} disabled={actionBusy.value} className={skillActionClass(d, isDisc)}>
						{skillActionLabel(d, isDisc, actionBusy.value)}
					</button>
					<button
						type="button"
						onClick={onClose}
						style={{ background: "none", border: "none", color: "var(--muted)", cursor: "pointer" }}
					>
						&times;
					</button>
				</div>
			</div>
			<SkillMetadata detail={d} />
			{d.description && <p style={{ margin: "0 0 8px", fontSize: ".82rem" }}>{d.description}</p>}
			{d.body_html && (
				<div
					style={{
						marginTop: "10px",
						border: "1px solid var(--border)",
						borderRadius: "var(--radius-sm)",
						background: "var(--surface2)",
					}}
				>
					<div
						style={{
							padding: "6px 10px",
							borderBottom: "1px solid var(--border)",
							fontSize: ".68rem",
							color: "var(--muted)",
							fontFamily: "var(--font-mono)",
							textTransform: "uppercase",
						}}
					>
						SKILL.md source
					</div>
					<div
						ref={bodyRef}
						className="skill-body-md"
						style={{ padding: "10px", maxHeight: "400px", overflowY: "auto", fontSize: ".8rem", lineHeight: 1.5 }}
					/>
				</div>
			)}
			{!d.body_html && d.body && (
				<div
					style={{
						marginTop: "10px",
						border: "1px solid var(--border)",
						borderRadius: "var(--radius-sm)",
						background: "var(--surface2)",
					}}
				>
					<pre
						style={{
							whiteSpace: "pre-wrap",
							wordBreak: "break-word",
							fontSize: ".78rem",
							fontFamily: "var(--font-mono)",
							margin: 0,
							padding: "10px",
							maxHeight: "400px",
							overflowY: "auto",
						}}
					>
						{d.body}
					</pre>
				</div>
			)}
		</div>
	);
}

function RepoCard({ repo }: { repo: RepoSummary }): VNode {
	const expanded = useSignal(false);
	const searchQuery = useSignal("");
	const searchResults = useSignal<SkillSummary[]>([]);
	const allSkills = useSignal<SkillSummary[]>([]);
	const searching = useSignal(false);
	const activeDetail = useSignal<SkillDetail | null>(null);
	const detailLoading = useSignal(false);
	const searchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
	const removingRepo = useSignal(false);
	const exportingRepo = useSignal(false);
	const unquarantiningRepo = useSignal(false);
	const isOrphan = repo.orphaned === true;
	const sourceLabel = isOrphan ? repo.repo_name : repo.source;
	const href = isOrphan ? null : repoHref(repo.source);
	function toggle(): void {
		const w = !expanded.value;
		expanded.value = w;
		if (w && !isOrphan && !allSkills.value.length) {
			searching.value = true;
			searchSkills(repo.source, "").then((r) => {
				allSkills.value = r;
				searching.value = false;
			});
		}
	}
	function onSearch(e: Event): void {
		if (isOrphan) return;
		const q = (e.target as HTMLInputElement).value;
		searchQuery.value = q;
		activeDetail.value = null;
		if (searchTimer.current) clearTimeout(searchTimer.current);
		if (!q.trim()) {
			searchResults.value = [];
			return;
		}
		searching.value = true;
		searchTimer.current = setTimeout(() => {
			searchSkills(repo.source, q.trim()).then((r) => {
				searchResults.value = r;
				searching.value = false;
			});
		}, 200);
	}
	function loadDetail(s: SkillSummary): void {
		detailLoading.value = true;
		sendRpc("skills.skill.detail", { source: repo.source, skill: s.name }).then((r) => {
			detailLoading.value = false;
			if (r?.ok) activeDetail.value = r.payload as SkillDetail;
			else showToast(`Failed: ${r?.error?.message || "unknown"}`, "error");
		});
	}
	const installingSkill = useSignal<string | null>(null);
	function quickInstall(sk: SkillSummary): void {
		if (installingSkill.value) {
			showToast("Another install is in progress, please wait\u2026", "error");
			return;
		}
		installingSkill.value = sk.name;
		sendRpc("skills.skill.enable", { source: repo.source, skill: sk.name }).then((r) => {
			installingSkill.value = null;
			if (r?.ok) {
				allSkills.value = allSkills.value.map((s) => (s.name === sk.name ? { ...s, enabled: true } : s));
				fetchAll();
			} else showToast(`Failed: ${r?.error?.message || "unknown"}`, "error");
		});
	}
	const installingAll = useSignal(false);
	function showInstallAllSuccess(count: number): void {
		if (count > 0) showToast(`Installed ${count} skill${count > 1 ? "s" : ""}`, "success");
	}
	async function installAllSkills(): Promise<void> {
		let skills = allSkills.value;
		if (!skills.length) {
			skills = await searchSkills(repo.source, "");
			allSkills.value = skills;
		}
		const unenabled = skills.filter((sk) => !sk.enabled);
		if (!unenabled.length) return;
		const yes = await requestConfirm(`You are about to enable ${unenabled.length} skills, are you sure?`, {
			confirmLabel: "Install All",
		});
		if (!yes) return;
		installingAll.value = true;
		const succeeded = new Set<string>();
		for (const sk of unenabled) {
			const r = await sendRpc("skills.skill.enable", { source: repo.source, skill: sk.name });
			if (r?.ok) {
				succeeded.add(sk.name);
			} else {
				showToast(`Failed to install ${sk.name}: ${r?.error?.message || "unknown"}`, "error");
			}
		}
		installingAll.value = false;
		showInstallAllSuccess(succeeded.size);
		// Re-fetch both the skill list and repo summary from the server
		// so counts are accurate (no optimistic update).
		const [freshSkills] = await Promise.all([searchSkills(repo.source, ""), fetchAllAsync()]);
		allSkills.value = freshSkills;
	}
	const displayed = searchQuery.value.trim() ? searchResults.value : allSkills.value;
	const unenabledCount =
		allSkills.value.length > 0
			? allSkills.value.filter((skill) => !skill.enabled).length
			: repo.skill_count - repo.enabled_count;

	function exportRepository(): void {
		exportingRepo.value = true;
		doExportBundle(repo.source, null).finally(() => {
			exportingRepo.value = false;
		});
	}

	function clearRepositoryQuarantine(): void {
		if (!S.connected || unquarantiningRepo.value) return;
		requestConfirm(`Clear quarantine for ${repo.source}?`, { confirmLabel: "Clear Quarantine" }).then((confirmed) => {
			if (!confirmed) return;
			unquarantiningRepo.value = true;
			doUnquarantine(repo.source).finally(() => {
				unquarantiningRepo.value = false;
			});
		});
	}

	function removeRepository(): void {
		removingRepo.value = true;
		sendRpc("skills.repos.remove", { source: repo.source }).then((response) => {
			removingRepo.value = false;
			if (response?.ok) fetchAll();
			else showToast(`Failed: ${response?.error?.message || "unknown"}`, "error");
		});
	}

	function renderTrustBadge(): VNode | null {
		if (repo.trusted_count == null || repo.skill_count === 0) return null;
		const trusted = repo.trusted_count === repo.skill_count;
		return (
			<span
				style={{
					fontSize: ".68rem",
					padding: "1px 5px",
					borderRadius: "var(--radius-sm)",
					background: trusted ? "var(--success-bg, rgba(34,197,94,.12))" : "var(--warning-bg, rgba(234,179,8,.12))",
					color: trusted ? "var(--success, #22c55e)" : "var(--warning, #eab308)",
				}}
			>
				{trusted ? "trusted" : `${repo.trusted_count}/${repo.skill_count} trusted`}
			</span>
		);
	}

	function renderSourceLabel(): VNode {
		const style = {
			fontFamily: "var(--font-mono)",
			fontSize: ".82rem",
			fontWeight: 500,
			color: "var(--text-strong)",
		};
		if (!href) return <span style={style}>{sourceLabel}</span>;
		return (
			<a href={href} target="_blank" rel="noopener noreferrer" style={{ ...style, textDecoration: "none" }}>
				{sourceLabel}
			</a>
		);
	}

	function renderRepositoryIdentity(): VNode {
		return (
			<div style={{ display: "flex", alignItems: "center", gap: "8px" }}>
				<button
					type="button"
					className="border-0 bg-transparent p-0 cursor-pointer"
					style={{ fontSize: ".65rem", color: "var(--muted)" }}
					aria-label={`${expanded.value ? "Collapse" : "Expand"} skills from ${sourceLabel}`}
					aria-expanded={expanded.value}
					onClick={toggle}
				>
					<span className={`block transition-transform duration-150 ${expanded.value ? "rotate-90" : ""}`}>
						{"\u25B6"}
					</span>
				</button>
				{!isOrphan && (
					<img
						src={orgAvatarUrl(repo.source)}
						alt=""
						style={{ width: "20px", height: "20px", borderRadius: "var(--radius-sm)" }}
					/>
				)}
				{renderSourceLabel()}
				<span style={{ fontSize: ".72rem", color: "var(--muted)" }}>
					{repo.enabled_count}/{repo.skill_count} enabled
				</span>
				{renderTrustBadge()}
			</div>
		);
	}

	function renderRepositoryActions(): VNode {
		return (
			<div style={{ display: "flex", gap: "6px" }}>
				{!isOrphan && unenabledCount > 0 && (
					<button
						type="button"
						className="provider-btn provider-btn-sm"
						disabled={installingAll.value}
						onClick={() => installAllSkills().catch(console.error)}
					>
						{installingAll.value ? "Installing\u2026" : "Install All"}
					</button>
				)}
				{!isOrphan && (
					<button
						type="button"
						className="provider-btn provider-btn-sm provider-btn-secondary"
						disabled={exportingRepo.value}
						onClick={exportRepository}
					>
						{exportingRepo.value ? "Exporting..." : "Export"}
					</button>
				)}
				{repo.quarantined && (
					<button
						type="button"
						className="provider-btn provider-btn-sm provider-btn-secondary"
						disabled={unquarantiningRepo.value}
						onClick={clearRepositoryQuarantine}
					>
						{unquarantiningRepo.value ? "Clearing..." : "Clear Quarantine"}
					</button>
				)}
				<button
					type="button"
					className="provider-btn provider-btn-sm provider-btn-danger"
					disabled={removingRepo.value}
					onClick={removeRepository}
				>
					{removingRepo.value ? "Removing..." : "Remove"}
				</button>
			</div>
		);
	}

	function renderRepositoryProvenance(): VNode | null {
		if (!(expanded.value && (repo.quarantined || repo.provenance))) return null;
		return (
			<div style={{ padding: "8px 12px", fontSize: ".78rem", color: "var(--muted)" }}>
				{repo.quarantined && (
					<div style={{ marginBottom: "6px", color: "var(--warning, #c77d00)", fontWeight: 600 }}>
						Quarantined{repo.quarantine_reason ? `: ${repo.quarantine_reason}` : ""}
					</div>
				)}
				{repo.provenance?.original_source && (
					<div>
						<strong>Original source:</strong> {repo.provenance.original_source}
					</div>
				)}
				{repo.provenance?.original_commit_sha && (
					<div>
						<strong>Original commit:</strong> <code>{shortSha(repo.provenance.original_commit_sha)}</code>
					</div>
				)}
				{repo.provenance?.imported_from && (
					<div>
						<strong>Imported from:</strong> <code>{repo.provenance.imported_from}</code>
					</div>
				)}
			</div>
		);
	}

	function renderBrowseSkill(skill: SkillSummary): VNode {
		const installing = installingSkill.value === skill.name;
		return (
			<div
				key={skill.name}
				className="skills-ac-item"
				style={{ display: "flex", alignItems: "center", justifyContent: "space-between" }}
			>
				<button
					type="button"
					className="border-0 bg-transparent p-0 text-left font-[inherit]"
					style={{ flex: 1, minWidth: 0, cursor: "pointer" }}
					onClick={() => loadDetail(skill)}
				>
					<span style={{ fontFamily: "var(--font-mono)", fontWeight: 500, color: "var(--text-strong)" }}>
						{skill.display_name || skill.name}
					</span>
					{skill.description && (
						<span style={{ color: "var(--muted)", fontSize: ".72rem", marginLeft: "6px" }}>{skill.description}</span>
					)}
				</button>
				<div style={{ display: "flex", gap: "4px", flexShrink: 0, marginLeft: "8px" }}>
					<button
						type="button"
						className="provider-btn provider-btn-sm provider-btn-secondary"
						onClick={() => loadDetail(skill)}
					>
						View
					</button>
					{skill.enabled ? (
						<span style={{ fontSize: ".72rem", padding: "4px 8px", color: "var(--success, #22c55e)", fontWeight: 500 }}>
							Installed
						</span>
					) : (
						<button
							type="button"
							className="provider-btn provider-btn-sm"
							disabled={installing}
							onClick={() => quickInstall(skill)}
						>
							{installing ? "Installing\u2026" : "Install"}
						</button>
					)}
				</div>
			</div>
		);
	}

	function renderRepositoryDetail(): VNode | null {
		if (!expanded.value) return null;
		return (
			<div className="skills-repo-detail" style={{ display: "block" }}>
				<div style={{ marginBottom: "8px" }}>
					<input
						type="text"
						placeholder={`Search skills in ${repo.source}\u2026`}
						value={searchQuery.value}
						disabled={isOrphan}
						onInput={onSearch}
						style={{
							width: "100%",
							padding: "6px 10px",
							border: "1px solid var(--border)",
							borderRadius: "var(--radius-sm)",
							background: "var(--surface)",
							color: "var(--text)",
							fontSize: ".8rem",
							fontFamily: "var(--font-mono)",
							boxSizing: "border-box",
						}}
					/>
				</div>
				{!activeDetail.value && displayed.length > 0 && (
					<div className="skills-browse-list">{displayed.map(renderBrowseSkill)}</div>
				)}
				{searching.value && !activeDetail.value && (
					<div style={{ padding: "8px", color: "var(--muted)", fontSize: ".78rem" }}>Searching...</div>
				)}
				{activeDetail.value && (
					<SkillDetailPanel
						detail={activeDetail.value}
						repoSource={repo.source}
						onClose={() => {
							activeDetail.value = null;
						}}
						onReload={() => loadDetail({ name: activeDetail.value?.name } as SkillSummary)}
					/>
				)}
			</div>
		);
	}

	return (
		<div className="skills-repo-card">
			<div className="skills-repo-header">
				{renderRepositoryIdentity()}
				{renderRepositoryActions()}
			</div>
			{renderRepositoryProvenance()}
			{renderRepositoryDetail()}
		</div>
	);
}

const bundledCategories = signal<BundledCategory[]>([]);
const bundledTotal = signal(0);

function fetchBundledCategories(): void {
	sendRpc("skills.bundled.categories", {}).then((res) => {
		if (res?.ok) {
			const payload = res.payload as { categories?: BundledCategory[]; total_skills?: number };
			bundledCategories.value = payload.categories || [];
			bundledTotal.value = payload.total_skills || 0;
		}
	});
}

function BundledCategoriesSection(): VNode | null {
	const cats = bundledCategories.value;
	const toggling = useSignal<string | null>(null);

	useEffect(() => {
		fetchBundledCategories();
	}, []);

	if (!cats.length) return null;

	function toggle(cat: BundledCategory): void {
		if (toggling.value) return;
		const newEnabled = !cat.enabled;
		toggling.value = cat.name;
		sendRpc("skills.bundled.toggle_category", { category: cat.name, enabled: newEnabled }).then((res) => {
			toggling.value = null;
			if (res?.ok) {
				bundledCategories.value = bundledCategories.value.map((c) =>
					c.name === cat.name ? { ...c, enabled: newEnabled } : c,
				);
				fetchAll();
			} else {
				showToast(`Failed: ${res?.error?.message || "unknown"}`, "error");
			}
		});
	}

	const enabledCount = cats.filter((c) => c.enabled).length;

	return (
		<div className="skills-section">
			<div className="flex items-center gap-3 mb-2">
				<h3 className="skills-section-title" style={{ margin: 0 }}>
					{t("skills:bundledTitle")}
					<span className="ml-2 text-xs font-normal text-[var(--muted)]">
						({enabledCount}/{cats.length} enabled)
					</span>
				</h3>
			</div>
			<p className="text-xs text-[var(--muted)] mb-3">{t("skills:bundledDescription")}</p>
			<div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-2">
				{cats.map((cat) => {
					const meta = CATEGORY_META[cat.name];
					const icon = meta?.icon || "\uD83D\uDCE6";
					return (
						<button
							key={cat.name}
							type="button"
							onClick={() => toggle(cat)}
							disabled={toggling.value === cat.name}
							className={`flex items-center gap-2 px-3 py-2 rounded-md border text-left cursor-pointer transition-colors ${
								cat.enabled
									? "border-[var(--accent)] bg-[var(--accent-bg,rgba(var(--accent-rgb,59,130,246),0.08))]"
									: "border-[var(--border)] bg-[var(--surface)] opacity-60"
							}`}
						>
							<span className="text-base shrink-0">{icon}</span>
							<div className="flex-1 min-w-0">
								<span className="text-xs font-medium text-[var(--text-strong)]">{categoryLabel(cat.name)}</span>
								<span className="text-xs text-[var(--muted)] ml-1">({cat.count})</span>
								{meta?.desc && <div className="text-xs text-[var(--muted)] truncate">{meta.desc}</div>}
							</div>
							{cat.enabled ? (
								<span className="icon icon-check-circle text-[var(--accent)] shrink-0" />
							) : (
								<span className="w-4 h-4 rounded-full border-2 border-[var(--border)] inline-block shrink-0" />
							)}
						</button>
					);
				})}
			</div>
		</div>
	);
}

function ReposSection(): VNode {
	return (
		<div className="skills-section">
			<h3 className="skills-section-title">Installed Repositories</h3>
			<div className="skills-section">
				{!repos.value?.length && (
					<div style={{ padding: "12px", color: "var(--muted)", fontSize: ".82rem" }}>No repositories installed.</div>
				)}
				{repos.value.map((r) => (
					<RepoCard key={r.source} repo={r} />
				))}
			</div>
		</div>
	);
}

function EnabledSkillsTable(): VNode | null {
	const s = enabledSkills.value;
	const map = skillRepoMap.value;
	const activeDetail = useSignal<SkillDetail | null>(null);
	const detailLoading = useSignal(false);
	const pending = useSignal<string | null>(null);
	const activeCategory = useSignal<string | null>(null);
	const searchQuery = useSignal("");
	if (!s?.length) return null;

	// Build sorted category list from skills
	const categories = computed(() => {
		const cats = new Set<string>();
		for (const sk of enabledSkills.value) {
			cats.add(sk.category || "other");
		}
		return Array.from(cats).sort();
	});

	// Filter skills by search query and active category
	const filtered = s.filter((sk) => {
		if (activeCategory.value && (sk.category || "other") !== activeCategory.value) return false;
		if (searchQuery.value) {
			const q = searchQuery.value.toLowerCase();
			return sk.name.toLowerCase().includes(q) || (sk.description || "").toLowerCase().includes(q);
		}
		return true;
	});

	function isDisc(sk: SkillSummary): boolean {
		return isDiscoveredSource(sk.source);
	}
	function doDisable(sk: SkillSummary): void {
		pending.value = sk.name;
		sendRpc("skills.skill.disable", { source: map[sk.name] || sk.source, skill: sk.name }).then((r) => {
			pending.value = null;
			if (r?.ok) {
				activeDetail.value = null;
				showToast(isDisc(sk) ? `Deleted ${sk.name}` : `Disabled ${sk.name}`, "success");
				fetchAll();
			} else showToast(`Failed: ${r?.error?.message || "unknown"}`, "error");
		});
	}
	function onDisable(sk: SkillSummary): void {
		if (pending.value) return;
		if (isDisc(sk) && sk.protected) {
			showToast("Protected", "error");
			return;
		}
		if (isDisc(sk)) {
			requestConfirm(`Delete "${sk.name}"?`, { confirmLabel: "Delete", danger: true }).then((y) => {
				if (y) doDisable(sk);
			});
			return;
		}
		doDisable(sk);
	}
	function loadDetail(sk: SkillSummary): void {
		if (activeDetail.value?.name === sk.name) {
			activeDetail.value = null;
			return;
		}
		detailLoading.value = true;
		sendRpc("skills.skill.detail", { source: map[sk.name] || sk.source, skill: sk.name }).then((r) => {
			detailLoading.value = false;
			if (r?.ok) activeDetail.value = r.payload as SkillDetail;
		});
	}

	function renderSkillAction(skill: SkillSummary): VNode | null {
		if (skill.source === SkillSource.Bundled) return null;
		const discovered = isDisc(skill);
		return (
			<button
				type="button"
				disabled={(discovered && skill.protected === true) || pending.value === skill.name}
				className={
					discovered
						? "provider-btn provider-btn-sm provider-btn-danger"
						: "provider-btn provider-btn-sm provider-btn-secondary"
				}
				onClick={(event) => {
					event.stopPropagation();
					onDisable(skill);
				}}
			>
				{pending.value === skill.name ? "..." : discovered ? "Delete" : "Disable"}
			</button>
		);
	}

	function renderActiveSkillDetail(skill: SkillSummary): VNode | null {
		if (activeDetail.value?.name !== skill.name) return null;
		return (
			<tr key={`${skill.name}-detail`}>
				<td colSpan={4} style={{ padding: 0, borderBottom: "1px solid var(--border)" }}>
					<SkillDetailPanel
						detail={activeDetail.value}
						repoSource={activeDetail.value.source}
						onClose={() => {
							activeDetail.value = null;
						}}
						onReload={() =>
							loadDetail({
								name: activeDetail.value?.name,
								source: activeDetail.value?.source,
							} as SkillSummary)
						}
					/>
				</td>
			</tr>
		);
	}

	function renderEnabledSkill(skill: SkillSummary): VNode {
		const active = activeDetail.value?.name === skill.name;
		return (
			<>
				<tr
					key={skill.name}
					className="cursor-pointer"
					style={{
						borderBottom: active ? "none" : "1px solid var(--border)",
						background: active ? "var(--bg-hover)" : undefined,
					}}
					onClick={() => loadDetail(skill)}
				>
					<td
						style={{
							padding: "8px 12px",
							fontWeight: 500,
							color: "var(--accent)",
							fontFamily: "var(--font-mono)",
						}}
					>
						{skill.name}
						{skill.category && !activeCategory.value && <span className="skills-category-badge">{skill.category}</span>}
					</td>
					<td style={{ padding: "8px 12px" }}>{skill.description || "\u2014"}</td>
					<td style={{ padding: "8px 12px" }}>
						<span className={isRepoSource(skill.source) ? "tier-badge" : "recommended-badge"}>{skill.source}</span>
					</td>
					<td style={{ padding: "8px 12px", textAlign: "right" }}>{renderSkillAction(skill)}</td>
				</tr>
				{renderActiveSkillDetail(skill)}
			</>
		);
	}
	return (
		<div className="skills-section">
			<div className="flex items-center gap-3 mb-2">
				<h3 className="skills-section-title" style={{ margin: 0 }}>
					Enabled Skills
					<span className="ml-2 text-xs font-normal text-[var(--muted)]">
						({filtered.length}
						{filtered.length !== s.length ? ` of ${s.length}` : ""})
					</span>
				</h3>
				<input
					type="text"
					placeholder="Search skills..."
					value={searchQuery.value}
					onInput={(e) => {
						searchQuery.value = (e.target as HTMLInputElement).value;
					}}
					className="skills-install-input"
					style={{ maxWidth: "240px", fontSize: ".78rem", padding: "4px 8px" }}
				/>
			</div>
			{categories.value.length > 1 && (
				<div className="flex flex-wrap gap-1.5 mb-3">
					<button
						type="button"
						className={`skills-category-pill ${activeCategory.value === null ? "active" : ""}`}
						onClick={() => {
							activeCategory.value = null;
						}}
					>
						All ({s.length})
					</button>
					{categories.value.map((cat) => {
						const count = s.filter((sk) => (sk.category || "other") === cat).length;
						return (
							<button
								type="button"
								key={cat}
								className={`skills-category-pill ${activeCategory.value === cat ? "active" : ""}`}
								onClick={() => {
									activeCategory.value = activeCategory.value === cat ? null : cat;
								}}
							>
								{cat} ({count})
							</button>
						);
					})}
				</div>
			)}
			<div className="skills-table-wrap">
				<table style={{ width: "100%", borderCollapse: "collapse", fontSize: ".82rem" }}>
					<thead>
						<tr style={{ borderBottom: "1px solid var(--border)", background: "var(--surface)" }}>
							<th
								style={{
									textAlign: "left",
									padding: "8px 12px",
									fontWeight: 500,
									color: "var(--muted)",
									fontSize: ".75rem",
									textTransform: "uppercase",
								}}
							>
								Name
							</th>
							<th
								style={{
									textAlign: "left",
									padding: "8px 12px",
									fontWeight: 500,
									color: "var(--muted)",
									fontSize: ".75rem",
									textTransform: "uppercase",
								}}
							>
								Description
							</th>
							<th
								style={{
									textAlign: "left",
									padding: "8px 12px",
									fontWeight: 500,
									color: "var(--muted)",
									fontSize: ".75rem",
									textTransform: "uppercase",
								}}
							>
								Source
							</th>
							<th />
						</tr>
					</thead>
					<tbody>{filtered.map(renderEnabledSkill)}</tbody>
				</table>
			</div>
		</div>
	);
}

const activeTab = signal("skills");

const skillsTabs = computed(() => {
	const enabledCats = bundledCategories.value.filter((c) => c.enabled).length;
	const totalCats = bundledCategories.value.length;
	return [
		{ id: "skills", label: "Skills", badge: enabledSkills.value.length || undefined },
		{ id: "categories", label: "Categories", badge: totalCats ? `${enabledCats}/${totalCats}` : undefined },
		{ id: "repositories", label: "Repositories", badge: repos.value.length || undefined },
	];
});

function SkillsPageComponent(): VNode {
	useEffect(() => {
		ensurePrefetch().then(() => fetchAll());
		fetchBundledCategories();
		const off = onEvent("skills.install.progress", (p: unknown) => {
			const d = p as Record<string, string>;
			if (!d?.op_id) return;
			if (d.phase === "start") startInstallProgress(d.source || "repository", d.op_id);
			else if (d.phase === "done") stopInstallProgress(d.op_id, true);
			else if (d.phase === "error") stopInstallProgress(d.op_id, false);
		});
		return off;
	}, []);
	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<div className="flex items-center gap-3">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Skills</h2>
				<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={fetchAll}>
					Refresh
				</button>
				<button
					type="button"
					className="provider-btn provider-btn-danger provider-btn-sm"
					onClick={emergencyDisableAllSkills}
				>
					Emergency Disable
				</button>
			</div>
			<p className="text-sm text-[var(--muted)]">
				SKILL.md-based skills.{" "}
				<a
					href="https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview"
					target="_blank"
					rel="noopener noreferrer"
					className="text-[var(--accent)]"
				>
					How to write a skill?
				</a>
			</p>
			<TabBar
				tabs={skillsTabs.value}
				active={activeTab.value}
				onChange={(id) => {
					activeTab.value = id;
				}}
			/>
			{activeTab.value === "skills" && (
				<>
					{loading.value && !enabledSkills.value.length && (
						<div style={{ padding: "24px", textAlign: "center", color: "var(--muted)" }}>Loading skills...</div>
					)}
					<EnabledSkillsTable />
				</>
			)}
			{activeTab.value === "categories" && <BundledCategoriesSection />}
			{activeTab.value === "repositories" && (
				<>
					<InstallBox />
					<InstallProgressBar />
					<FeaturedSection />
					<ReposSection />
				</>
			)}
		</div>
	);
}

let _skillsContainer: HTMLElement | null = null;
export function initSkills(container: HTMLElement): void {
	_skillsContainer = container;
	container.style.cssText = "flex-direction:column;padding:0;overflow:hidden;";
	render(
		<>
			<SkillsPageComponent />
			<ConfirmDialog />
		</>,
		container,
	);
}
export function teardownSkills(): void {
	if (_skillsContainer) render(null, _skillsContainer);
	_skillsContainer = null;
}
registerPage(routes.skills, initSkills, teardownSkills);
