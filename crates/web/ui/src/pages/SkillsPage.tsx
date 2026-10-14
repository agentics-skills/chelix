// ── Skills page (Preact + Signals) ───────────────────────────
// Note: body_html is server-rendered trusted content from SKILL.md
// processed by pulldown-cmark on the Rust gateway side.

import { computed, signal, useSignal } from "@preact/signals";
import type { VNode } from "preact";
import { render } from "preact";
import { useEffect, useRef } from "preact/hooks";
import { TabBar } from "../components/forms/Tabs";
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
	SkillSource,
} from "../types/skill-source";
import { ConfirmDialog, requestConfirm } from "../ui";
import { type SkillFileDocument, SkillFileMetadata } from "./skills/SkillFileMetadata";

// ── Types ────────────────────────────────────────────────────

interface SkillSummary {
	name: string;
	description?: string;
	category?: string;
	source?: string;
	enabled?: boolean;
	protected?: boolean;
	display_name?: string;
}
interface SkillDetail extends SkillSummary {
	body?: string;
	body_html?: string;
	compatibility?: string;
	license?: string;
	file: SkillFileDocument;
}
interface ToastItem {
	id: number;
	message: string;
	type: string;
}
const enabledSkills = signal<SkillSummary[]>([]);
const loading = signal(false);
const toasts = signal<ToastItem[]>([]);
let toastId = 0;

let prefetchPromise: Promise<unknown> | null = null;
function ensurePrefetch(): Promise<unknown> {
	if (!prefetchPromise)
		prefetchPromise = fetch("/api/skills")
			.then((r) => r.json())
			.then((data) => {
				if (data.skills) enabledSkills.value = data.skills;
				return data;
			})
			.catch(() => null);
	return prefetchPromise;
}

function showToast(message: string, type: string): void {
	const id = ++toastId;
	toasts.value = toasts.value.concat([{ id, message, type }]);
	setTimeout(() => {
		toasts.value = toasts.value.filter((toast) => toast.id !== id);
	}, 4000);
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
			loading.value = false;
			updateNavCount("skills", (data.skills || []).length);
		})
		.catch(() => {
			loading.value = false;
		});
}

function skillActionClass(detail: SkillDetail, discovered: boolean): string {
	if (discovered && detail.enabled) return "provider-btn provider-btn-sm provider-btn-danger";
	if (detail.enabled) return "provider-btn provider-btn-sm provider-btn-secondary";
	return "provider-btn provider-btn-sm";
}

function skillActionLabel(detail: SkillDetail, discovered: boolean, busy: boolean): string {
	if (busy) return "...";
	if (discovered && detail.enabled) return "Delete";
	return detail.enabled ? "Disable" : "Enable";
}

function SkillDetailPanel({
	detail: d,
	source,
	onClose,
	onReload,
}: {
	detail: SkillDetail;
	source?: string;
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
		sendRpc(d.enabled ? "skills.skill.disable" : "skills.skill.enable", { source, skill: d.name }).then((r) => {
			actionBusy.value = false;
			if (r?.ok) {
				if (isDisc) onClose();
				fetchAll();
				onReload?.();
			} else showToast(`Failed: ${r?.error?.message || "unknown"}`, "error");
		});
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
					<SkillFileMetadata file={d.file} />
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
					<SkillFileMetadata file={d.file} />
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
						{d.file.body}
					</pre>
				</div>
			)}
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
			<div className="settings-columns">
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

function EnabledSkillsTable(): VNode | null {
	const s = enabledSkills.value;
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
		sendRpc("skills.skill.disable", { source: sk.source, skill: sk.name }).then((r) => {
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
		sendRpc("skills.skill.detail", { source: sk.source, skill: sk.name }).then((r) => {
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
						source={activeDetail.value.source}
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
						<span className="recommended-badge">{skill.source}</span>
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
					className="skills-search-input"
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
	];
});

function SkillsPageComponent(): VNode {
	useEffect(() => {
		ensurePrefetch().then(() => fetchAll());
		fetchBundledCategories();
	}, []);
	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<div className="flex items-center gap-3">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Skills</h2>
				<button type="button" className="provider-btn provider-btn-secondary provider-btn-sm" onClick={fetchAll}>
					Refresh
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
