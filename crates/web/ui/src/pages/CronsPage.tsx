// ── Crons page (Preact + Signals) ──────────────────────────

import { signal, useSignal } from "@preact/signals";
import type { TargetedSubmitEvent, VNode } from "preact";
import { render } from "preact";
import { useEffect } from "preact/hooks";
import { fetchChannelStatus } from "../channel-utils";
import * as gon from "../gon";
import { refresh as refreshGon } from "../gon";
import { sendRpc } from "../helpers";
import { updateNavCount } from "../nav-counts";
import { models as modelsSig } from "../stores/model-store";
import type { CronJob, CronPayload, CronSchedule } from "../types/gon";
import { ComboSelect, ConfirmDialog, Modal, ModelSelect, requestConfirm } from "../ui";

// ── Types ────────────────────────────────────────────────────

interface CronStatusInfo {
	running: boolean;
	jobCount: number;
	enabledCount: number;
	nextRunAtMs?: number;
}

interface HeartbeatConfig {
	enabled?: boolean;
	every?: string;
	model?: string;
	prompt?: string;
	ack_max_chars?: number;
	deliver?: boolean;
	channel?: string;
	to?: string;
	active_hours?: { start?: string; end?: string; timezone?: string };
}

interface HeartbeatStatusInfo {
	promptSource?: string;
	job?: CronJob;
}

interface CronRun {
	startedAtMs: number;
	status: string;
	durationMs: number;
	error?: string;
	inputTokens?: number;
	outputTokens?: number;
}

interface RunsHistory {
	jobId: string;
	jobName: string;
	runs: CronRun[] | null;
}

interface ChannelAccount {
	account_id: string;
	name?: string;
	status: string;
}

// ── Signals ──────────────────────────────────────────────────

const initialCrons = (gon.get("crons") as CronJob[] | null) || [];
const cronJobs = signal<CronJob[]>(initialCrons);
const cronStatus = signal<CronStatusInfo | null>(gon.get("cron_status") as CronStatusInfo | null);
if (initialCrons.length) updateNavCount("crons", initialCrons.filter((j) => j.enabled).length);
const runsHistory = signal<RunsHistory | null>(null);
const showModal = signal(false);
const editingJob = signal<CronJob | null>(null);
const activeSection = signal<"jobs" | "heartbeat">("jobs");
let _cronsContainer: HTMLElement | null = null;

// Heartbeat state
const heartbeatStatus = signal<HeartbeatStatusInfo | null>(null);
const heartbeatRuns = signal<CronRun[] | null>((gon.get("heartbeat_runs") as CronRun[] | null) || []);
const heartbeatSaving = signal(false);
const heartbeatRunning = signal(false);
const heartbeatConfig = signal<HeartbeatConfig>((gon.get("heartbeat_config") as HeartbeatConfig | null) || {});
const channelAccounts = signal<ChannelAccount[]>([]);
const heartbeatModel = signal((gon.get("heartbeat_config") as HeartbeatConfig | null)?.model || "");
function loadChannelAccounts(): void {
	fetchChannelStatus().then((res: unknown) => {
		const r = res as { ok?: boolean; payload?: { channels?: ChannelAccount[] } } | null;
		if (r?.ok) channelAccounts.value = (r.payload?.channels || []).filter((c) => c.status === "connected");
	});
}
function loadHeartbeatStatus(): void {
	sendRpc("heartbeat.status", {}).then((res) => {
		if (res?.ok) heartbeatStatus.value = res.payload as HeartbeatStatusInfo;
	});
}
function findHeartbeatJob(): CronJob | null {
	return cronJobs.value.find((j) => j.name === "__heartbeat__") || (heartbeatStatus.value?.job as CronJob) || null;
}
function loadHeartbeatRuns(): void {
	if (!findHeartbeatJob()) {
		heartbeatRuns.value = heartbeatRuns.value || [];
		return;
	}
	heartbeatRuns.value = null;
	sendRpc("heartbeat.runs", { limit: 10 }).then((res) => {
		heartbeatRuns.value = res?.ok ? (res.payload as CronRun[]) || [] : [];
	});
}
function heartbeatRunBlockedReason(cfg: HeartbeatConfig, promptSource: string, job: CronJob | null): string | null {
	if (cfg.enabled === false) return "Heartbeat is disabled. Enable it to allow manual runs.";
	if (promptSource === "default")
		return "Heartbeat is inactive because no prompt is configured. Add a custom prompt or write actionable content in HEARTBEAT.md.";
	if (!job) return "Heartbeat has no active cron job yet. Save the heartbeat settings to recreate it.";
	return null;
}
function loadStatus(): void {
	sendRpc("cron.status", {}).then((res) => {
		if (res?.ok) cronStatus.value = res.payload as CronStatusInfo;
	});
}
function loadJobs(): void {
	sendRpc("cron.list", {}).then((res) => {
		if (res?.ok) {
			cronJobs.value = (res.payload as CronJob[]) || [];
			updateNavCount("crons", cronJobs.value.filter((j) => j.enabled).length);
		}
	});
}

function formatSchedule(sched: CronSchedule): string {
	if (sched.kind === "at") return `At ${new Date(sched.atMs).toLocaleString()}`;
	if (sched.kind === "every") {
		const ms = sched.everyMs;
		if (ms >= 3600000) return `Every ${ms / 3600000}h`;
		if (ms >= 60000) return `Every ${ms / 60000}m`;
		return `Every ${ms / 1000}s`;
	}
	if (sched.kind === "cron") return sched.expr + (sched.tz ? ` (${sched.tz})` : "");
	return JSON.stringify(sched);
}

function formatTokens(n: number | null | undefined): string | null {
	if (n == null) return null;
	if (n >= 1000) return `${(n / 1000).toFixed(1).replace(/\.0$/, "")}K`;
	return String(n);
}

function TokenBadge({ run }: { run: CronRun }): VNode | null {
	if (run.inputTokens == null && run.outputTokens == null) return null;
	const parts: string[] = [];
	if (run.inputTokens != null) parts.push(`${formatTokens(run.inputTokens)} in`);
	if (run.outputTokens != null) parts.push(`${formatTokens(run.outputTokens)} out`);
	return <span className="text-xs text-[var(--muted)] font-mono">{parts.join(" / ")}</span>;
}

function HeartbeatRunsList({ runs }: { runs: CronRun[] | null }): VNode {
	if (runs === null) return <div className="text-xs text-[var(--muted)]">Loading&hellip;</div>;
	if (runs.length === 0) return <div className="text-xs text-[var(--muted)]">No runs yet.</div>;
	return (
		<div className="flex flex-col">
			{runs.map((run) => (
				<div
					key={run.startedAtMs}
					className="flex items-center gap-3 py-2 border-b border-[var(--border)]"
					style={{ minHeight: "36px" }}
				>
					<span className={`status-dot ${run.status === "ok" ? "connected" : ""}`} />
					<span className={`cron-badge ${run.status}`}>{run.status}</span>
					<span className="text-xs text-[var(--muted)] font-mono">{run.durationMs}ms</span>
					<TokenBadge run={run} />
					{run.error && <span className="text-xs text-[var(--error)] truncate">{run.error}</span>}
					<span className="flex-1" />
					<span className="text-xs text-[var(--muted)]">
						<time data-epoch-ms={run.startedAtMs}>{new Date(run.startedAtMs).toISOString()}</time>
					</span>
				</div>
			))}
		</div>
	);
}

function HeartbeatJobStatus({ job }: { job: CronJob | null }): VNode | null {
	if (!job) return null;
	const statusDotClass = job.enabled ? "connected" : "";
	return (
		<div className="info-bar" style={{ marginTop: "16px", marginBottom: "16px" }}>
			<span className="info-field">
				<span className={`status-dot ${statusDotClass}`} />
				<span className="info-label">{job.enabled ? "Enabled" : "Disabled"}</span>
			</span>
			{job.state?.lastStatus && (
				<span className="info-field">
					<span className="info-label">Last:</span>
					<span className={`cron-badge ${job.state.lastStatus}`}>{job.state.lastStatus}</span>
				</span>
			)}
			{job.state?.nextRunAtMs && (
				<span className="info-field">
					<span className="info-label">Next:</span>
					<span className="info-value">
						<time data-epoch-ms={job.state.nextRunAtMs}>{new Date(job.state.nextRunAtMs).toLocaleString()}</time>
					</span>
				</span>
			)}
		</div>
	);
}

function defaultModelPlaceholder(): string {
	if (!modelsSig.value.length) return "(server default)";
	const m = modelsSig.value[0] as unknown as { displayName?: string; id: string };
	return `(default: ${m.displayName || m.id})`;
}

const systemTimezone = Intl.DateTimeFormat().resolvedOptions().timeZone;
function cronTimezoneHelpText(): string {
	return systemTimezone
		? `Leave blank to use UTC. Enter ${systemTimezone} to use your local timezone.`
		: "Leave blank to use UTC. Enter a timezone like Europe/Paris to use your local timezone.";
}

function collectHeartbeatForm(form: Element): HeartbeatConfig {
	return {
		enabled: (form.querySelector("[data-hb=enabled]") as HTMLInputElement).checked,
		every: (form.querySelector("[data-hb=every]") as HTMLInputElement).value.trim() || "30m",
		model: heartbeatModel.value || undefined,
		prompt: (form.querySelector("[data-hb=prompt]") as HTMLTextAreaElement).value.trim() || undefined,
		ack_max_chars: parseInt((form.querySelector("[data-hb=ackMax]") as HTMLInputElement).value, 10) || 300,
		deliver: (form.querySelector("[data-hb=deliver]") as HTMLInputElement).checked,
		channel: (form.querySelector("[data-hb=channel]") as HTMLInputElement).value.trim() || undefined,
		to: (form.querySelector("[data-hb=to]") as HTMLInputElement).value.trim() || undefined,
		active_hours: {
			start: (form.querySelector("[data-hb=ahStart]") as HTMLInputElement).value.trim() || "08:00",
			end: (form.querySelector("[data-hb=ahEnd]") as HTMLInputElement).value.trim() || "24:00",
			timezone: (form.querySelector("[data-hb=ahTz]") as HTMLSelectElement).value.trim() || "local",
		},
	};
}

function heartbeatPromptSourceText(promptSource: string): string {
	if (promptSource === "config") return "config custom prompt";
	if (promptSource === "heartbeat_md") return "HEARTBEAT.md";
	return "none (heartbeat inactive)";
}

function heartbeatActiveHoursEnd(configuredEnd: string | undefined): string {
	if (!configuredEnd || configuredEnd === "24:00") return "23:59";
	return configuredEnd;
}

function HeartbeatSection(): VNode {
	const cfg = heartbeatConfig.value;
	const saving = heartbeatSaving.value;
	const promptSource = heartbeatStatus.value?.promptSource || "default";
	const job = findHeartbeatJob();
	const runBlockedReason = heartbeatRunBlockedReason(cfg, promptSource, job);

	function onSave(e: TargetedSubmitEvent<HTMLFormElement>): void {
		e.preventDefault();
		const updated = collectHeartbeatForm(e.currentTarget);
		heartbeatSaving.value = true;
		sendRpc("heartbeat.update", updated).then((res) => {
			heartbeatSaving.value = false;
			if (res?.ok) {
				heartbeatConfig.value = updated;
				heartbeatModel.value = updated.model || "";
				refreshGon();
				loadHeartbeatStatus();
				loadJobs();
				loadStatus();
			}
		});
	}

	function onRunNow(): void {
		if (runBlockedReason) return;
		heartbeatRunning.value = true;
		sendRpc("heartbeat.run", {}).then(() => {
			heartbeatRunning.value = false;
			loadHeartbeatStatus();
			loadHeartbeatRuns();
			loadJobs();
			loadStatus();
		});
	}

	function onToggleEnabled(e: Event): void {
		const newEnabled = (e.target as HTMLInputElement).checked;
		const updated = { ...cfg, enabled: newEnabled };
		sendRpc("heartbeat.update", updated).then((res) => {
			if (res?.ok) {
				heartbeatConfig.value = updated;
				refreshGon();
				loadHeartbeatStatus();
				loadJobs();
				loadStatus();
			}
		});
	}

	const running = heartbeatRunning.value;
	const runNowDisabled = running || !!runBlockedReason;
	const promptSourceText = heartbeatPromptSourceText(promptSource);

	return (
		<form className="heartbeat-form" style={{ maxWidth: "600px" }} onSubmit={onSave}>
			<div className="flex items-center justify-between mb-2">
				<div className="flex items-center gap-3">
					<h2 className="text-lg font-medium text-[var(--text-strong)]">Heartbeat</h2>
					<label className="cron-toggle">
						<input data-hb="enabled" type="checkbox" checked={cfg.enabled !== false} onChange={onToggleEnabled} />
						<span className="cron-slider" />
					</label>
					<span className="text-xs text-[var(--muted)]">Enable</span>
				</div>
				<button
					type="button"
					className="provider-btn provider-btn-secondary"
					onClick={onRunNow}
					disabled={runNowDisabled}
					title={runBlockedReason || undefined}
				>
					{running ? "Running\u2026" : "Run Now"}
				</button>
			</div>
			<p className="text-sm text-[var(--muted)] mb-4">
				Periodic AI check-in that monitors your environment and reports status.
			</p>
			{runBlockedReason && (
				<div className="alert-info-text max-w-form mb-4">
					<span className="alert-label-info">Heartbeat inactive:</span> {runBlockedReason}
				</div>
			)}
			<HeartbeatJobStatus job={job} />

			{/* Schedule */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<h3 className="text-sm font-medium text-[var(--text-strong)] mb-3">Schedule</h3>
				<div className="grid gap-4" style={{ gridTemplateColumns: "1fr 1fr" }}>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">Interval</span>
							<input data-hb="every" className="provider-key-input" placeholder="30m" value={cfg.every || "30m"} />
						</label>
					</div>
					<div>
						<span className="block text-xs text-[var(--muted)] mb-1">Model</span>
						<ModelSelect
							ariaLabel="Model"
							models={modelsSig.value}
							value={heartbeatModel.value}
							onChange={(v: string) => {
								heartbeatModel.value = v;
							}}
							placeholder={defaultModelPlaceholder()}
						/>
					</div>
				</div>
			</div>

			{/* Prompt */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<h3 className="text-sm font-medium text-[var(--text-strong)] mb-3">Prompt</h3>
				<label>
					<span className="block text-xs text-[var(--muted)] mb-1">Custom Prompt (optional)</span>
					<textarea
						data-hb="prompt"
						className="provider-key-input textarea-sm"
						placeholder="Leave blank to use default heartbeat prompt"
					>
						{cfg.prompt || ""}
					</textarea>
				</label>
				<p className="text-xs text-[var(--muted)] mt-2">
					Leave this empty to use <code>HEARTBEAT.md</code> in your workspace root. If that file exists but is
					empty/comments-only, heartbeat LLM runs are skipped to save tokens.
				</p>
				<p className="text-xs text-[var(--muted)] mt-1">
					Effective prompt source: <span className="text-[var(--text)]">{promptSourceText}</span>
				</p>
				<div className="grid gap-4 mt-3" style={{ gridTemplateColumns: "1fr" }}>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">Max Response Characters</span>
							<input
								data-hb="ackMax"
								className="provider-key-input"
								type="number"
								min="50"
								value={cfg.ack_max_chars || 300}
							/>
						</label>
					</div>
				</div>
			</div>

			{/* Delivery */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<h3 className="text-sm font-medium text-[var(--text-strong)] mb-3">Delivery</h3>
				<p className="text-xs text-[var(--muted)] mb-3">Send heartbeat replies to a channel/chat destination.</p>
				<div className="flex items-center gap-3 mb-3">
					<label className="cron-toggle">
						<input data-hb="deliver" type="checkbox" checked={cfg.deliver === true} />
						<span className="cron-slider" />
					</label>
					<span className="text-sm text-[var(--text)]">Deliver to channel</span>
				</div>
				<div className="grid gap-4" style={{ gridTemplateColumns: "1fr 1fr" }}>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">Channel Account</span>
							<input data-hb="channel" className="provider-key-input" placeholder="my-bot" value={cfg.channel || ""} />
						</label>
					</div>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">Chat ID</span>
							<input data-hb="to" className="provider-key-input" placeholder="123456789" value={cfg.to || ""} />
						</label>
					</div>
				</div>
				<p className="text-xs text-[var(--muted)] mt-2">
					Required when delivery is enabled. Account is your configured channel account id, chat ID is the destination
					recipient/group id.
				</p>
			</div>

			{/* Active Hours */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<h3 className="text-sm font-medium text-[var(--text-strong)] mb-3">Active Hours</h3>
				<p className="text-xs text-[var(--muted)] mb-3">Only run heartbeat during these hours.</p>
				<div className="grid gap-4" style={{ gridTemplateColumns: "1fr 1fr" }}>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">Start</span>
							<input
								data-hb="ahStart"
								type="time"
								className="provider-key-input"
								value={cfg.active_hours?.start || "08:00"}
							/>
						</label>
					</div>
					<div>
						<label>
							<span className="block text-xs text-[var(--muted)] mb-1">End</span>
							<input
								data-hb="ahEnd"
								type="time"
								className="provider-key-input"
								value={heartbeatActiveHoursEnd(cfg.active_hours?.end)}
							/>
						</label>
					</div>
				</div>
				<div className="mt-3">
					<label>
						<span className="block text-xs text-[var(--muted)] mb-1">Timezone</span>
						<select data-hb="ahTz" className="provider-key-input">
							<option value="local" selected={!cfg.active_hours?.timezone || cfg.active_hours?.timezone === "local"}>
								Local ({systemTimezone})
							</option>
							<option value="UTC" selected={cfg.active_hours?.timezone === "UTC"}>
								UTC
							</option>
							<option value="America/New_York" selected={cfg.active_hours?.timezone === "America/New_York"}>
								America/New_York (EST/EDT)
							</option>
							<option value="America/Chicago" selected={cfg.active_hours?.timezone === "America/Chicago"}>
								America/Chicago (CST/CDT)
							</option>
							<option value="America/Denver" selected={cfg.active_hours?.timezone === "America/Denver"}>
								America/Denver (MST/MDT)
							</option>
							<option value="America/Los_Angeles" selected={cfg.active_hours?.timezone === "America/Los_Angeles"}>
								America/Los_Angeles (PST/PDT)
							</option>
							<option value="Europe/London" selected={cfg.active_hours?.timezone === "Europe/London"}>
								Europe/London (GMT/BST)
							</option>
							<option value="Europe/Paris" selected={cfg.active_hours?.timezone === "Europe/Paris"}>
								Europe/Paris (CET/CEST)
							</option>
							<option value="Europe/Berlin" selected={cfg.active_hours?.timezone === "Europe/Berlin"}>
								Europe/Berlin (CET/CEST)
							</option>
							<option value="Asia/Tokyo" selected={cfg.active_hours?.timezone === "Asia/Tokyo"}>
								Asia/Tokyo (JST)
							</option>
							<option value="Asia/Shanghai" selected={cfg.active_hours?.timezone === "Asia/Shanghai"}>
								Asia/Shanghai (CST)
							</option>
							<option value="Asia/Singapore" selected={cfg.active_hours?.timezone === "Asia/Singapore"}>
								Asia/Singapore (SGT)
							</option>
							<option value="Australia/Sydney" selected={cfg.active_hours?.timezone === "Australia/Sydney"}>
								Australia/Sydney (AEST/AEDT)
							</option>
						</select>
					</label>
				</div>
			</div>

			{/* Recent Runs */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<h3 className="text-sm font-medium text-[var(--text-strong)] mb-3">Recent Runs</h3>
				<HeartbeatRunsList runs={heartbeatRuns.value} />
			</div>

			{/* Save */}
			<div style={{ marginTop: "24px", borderTop: "1px solid var(--border)", paddingTop: "16px" }}>
				<button type="submit" className="provider-btn" disabled={saving}>
					{saving ? "Saving\u2026" : "Save"}
				</button>
			</div>
		</form>
	);
}

// ── Cron Jobs ────────────────────────────────────────────────

function StatusBar(): VNode {
	const s = cronStatus.value;
	if (!s) return <div className="cron-status-bar">Loading&hellip;</div>;
	const parts = [
		s.running ? "Running" : "Stopped",
		`${s.jobCount} job${s.jobCount === 1 ? "" : "s"}`,
		`${s.enabledCount} enabled`,
	];
	if (s.nextRunAtMs) parts.push(`next: ${new Date(s.nextRunAtMs).toLocaleString()}`);
	return <div className="cron-status-bar">{parts.join(" \u2022 ")}</div>;
}

function CronJobRow({ job }: { job: CronJob }): VNode {
	const modelLabel = job.payload.kind === "agentTurn" ? job.payload.model || "default" : "\u2014";
	const deliveryLabel =
		job.payload.kind === "agentTurn" && job.payload.deliver && job.payload.channel
			? `\u2192 ${job.payload.channel}`
			: null;
	function onToggle(e: Event): void {
		sendRpc("cron.update", { id: job.id, patch: { enabled: (e.target as HTMLInputElement).checked } }).then(() => {
			loadJobs();
			loadStatus();
		});
	}
	function onRun(): void {
		sendRpc("cron.run", { id: job.id, force: true }).then(() => {
			loadJobs();
			loadStatus();
		});
	}
	function onDelete(): void {
		requestConfirm(`Delete job '${job.name}'?`).then((yes) => {
			if (yes)
				sendRpc("cron.remove", { id: job.id }).then(() => {
					loadJobs();
					loadStatus();
				});
		});
	}
	function onHistory(): void {
		runsHistory.value = { jobId: job.id, jobName: job.name, runs: null };
		sendRpc("cron.runs", { id: job.id }).then((res) => {
			if (res?.ok) runsHistory.value = { jobId: job.id, jobName: job.name, runs: (res.payload as CronRun[]) || [] };
		});
	}

	return (
		<tr>
			<td>{job.name}</td>
			<td className="cron-mono">{formatSchedule(job.schedule)}</td>
			<td className="cron-mono">{modelLabel}</td>
			<td className="cron-mono">{deliveryLabel ? <span className="text-xs">{deliveryLabel}</span> : "\u2014"}</td>
			<td className="cron-mono">
				{job.state?.nextRunAtMs ? (
					<time data-epoch-ms={job.state.nextRunAtMs}>{new Date(job.state.nextRunAtMs).toISOString()}</time>
				) : (
					"\u2014"
				)}
			</td>
			<td>
				{job.state?.lastStatus ? (
					<span className={`cron-badge ${job.state.lastStatus}`}>{job.state.lastStatus}</span>
				) : (
					"\u2014"
				)}
			</td>
			<td className="cron-actions">
				<button
					type="button"
					className="cron-action-btn"
					onClick={() => {
						editingJob.value = job;
						showModal.value = true;
					}}
				>
					Edit
				</button>
				<button type="button" className="cron-action-btn" onClick={onRun}>
					Run
				</button>
				<button type="button" className="cron-action-btn" onClick={onHistory}>
					History
				</button>
				<button type="button" className="cron-action-btn cron-action-danger" onClick={onDelete}>
					Delete
				</button>
			</td>
			<td>
				<label className="cron-toggle">
					<input type="checkbox" checked={job.enabled} onChange={onToggle} />
					<span className="cron-slider" />
				</label>
			</td>
		</tr>
	);
}

function CronJobTable(): VNode {
	const jobs = cronJobs.value.filter((j) => !j.system);
	if (jobs.length === 0) return <div className="text-sm text-[var(--muted)]">No cron jobs configured.</div>;
	return (
		<table className="cron-table">
			<thead>
				<tr>
					<th>Name</th>
					<th>Schedule</th>
					<th>Model</th>
					<th>Delivery</th>
					<th>Next Run</th>
					<th>Last Status</th>
					<th>Actions</th>
					<th>Enabled</th>
				</tr>
			</thead>
			<tbody>
				{jobs.map((job) => (
					<CronJobRow key={job.id} job={job} />
				))}
			</tbody>
		</table>
	);
}

function RunHistoryPanel(): VNode | null {
	const h = runsHistory.value;
	if (!h) return null;
	return (
		<div className="mb-md">
			<div className="flex items-center justify-between mb-md">
				<span className="text-sm font-medium text-[var(--text-strong)]">Run History: {h.jobName}</span>
				<button
					type="button"
					className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none hover:text-[var(--text)]"
					onClick={() => {
						runsHistory.value = null;
					}}
				>
					&times; Close
				</button>
			</div>
			{h.runs === null && <div className="text-sm text-[var(--muted)]">Loading&hellip;</div>}
			{h.runs !== null && h.runs.length === 0 && <div className="text-xs text-[var(--muted)]">No runs yet.</div>}
			{h.runs?.map((run) => (
				<div className="cron-run-item" key={run.startedAtMs}>
					<span className="text-xs text-[var(--muted)]">
						<time data-epoch-ms={run.startedAtMs}>{new Date(run.startedAtMs).toISOString()}</time>
					</span>
					<span className={`cron-badge ${run.status}`}>{run.status}</span>
					<span className="text-xs text-[var(--muted)]">{run.durationMs}ms</span>
					<TokenBadge run={run} />
					{run.error && <span className="text-xs text-[var(--error)]">{run.error}</span>}
				</div>
			))}
		</div>
	);
}

function parseScheduleFromForm(
	kind: string,
	signals: Pick<CronModalDraft, "schedAtTimestamp" | "schedEverySecs" | "schedCronExpr" | "schedCronTz">,
): { schedule?: CronSchedule; error?: string } {
	if (kind === "at") {
		const ts = new Date(signals.schedAtTimestamp).getTime();
		if (Number.isNaN(ts)) return { error: "at" };
		return { schedule: { kind: "at", atMs: ts } };
	}
	if (kind === "every") {
		const secs = parseInt(signals.schedEverySecs, 10);
		if (Number.isNaN(secs) || secs <= 0) return { error: "every" };
		return { schedule: { kind: "every", everyMs: secs * 1000 } };
	}
	const expr = signals.schedCronExpr.trim();
	if (!expr) return { error: "cron" };
	const schedule: CronSchedule = { kind: "cron", expr };
	const tz = signals.schedCronTz.trim();
	if (tz) schedule.tz = tz;
	return { schedule };
}

interface CronModalDraft {
	schedKind: string;
	jobModel: string;
	jobName: string;
	payloadKind: string;
	sessionTarget: string;
	messageText: string;
	deleteAfterRun: boolean;
	jobEnabled: boolean;
	deliverToChannel: boolean;
	deliverChannel: string;
	deliverTo: string;
	schedCronExpr: string;
	schedCronTz: string;
	schedEverySecs: string;
	schedAtTimestamp: string;
}

interface CronJobFields {
	name: string;
	schedule: CronSchedule;
	payload: CronPayload;
	sessionTarget: string;
	deleteAfterRun: boolean;
	enabled: boolean;
}

type CronSaveCommand = { ok: true; fields: CronJobFields } | { ok: false; errorField: string };

function emptyCronModalDraft(): CronModalDraft {
	return {
		schedKind: "cron",
		jobModel: "",
		jobName: "",
		payloadKind: "systemEvent",
		sessionTarget: "main",
		messageText: "",
		deleteAfterRun: false,
		jobEnabled: true,
		deliverToChannel: false,
		deliverChannel: "",
		deliverTo: "",
		schedCronExpr: "",
		schedCronTz: "",
		schedEverySecs: "",
		schedAtTimestamp: "",
	};
}

function cronModalDraft(job: CronJob | null): CronModalDraft {
	if (!job) return emptyCronModalDraft();
	const agentPayload = job.payload.kind === "agentTurn" ? job.payload : null;
	return {
		schedKind: job.schedule.kind,
		jobModel: agentPayload?.model || "",
		jobName: job.name,
		payloadKind: job.payload.kind,
		sessionTarget: job.sessionTarget || "main",
		messageText: job.payload.kind === "systemEvent" ? job.payload.text : job.payload.message,
		deleteAfterRun: !!job.deleteAfterRun,
		jobEnabled: job.enabled,
		deliverToChannel: !!agentPayload?.deliver,
		deliverChannel: agentPayload?.channel || "",
		deliverTo: agentPayload?.to || "",
		schedCronExpr: job.schedule.kind === "cron" ? job.schedule.expr : "",
		schedCronTz: job.schedule.kind === "cron" ? job.schedule.tz || "" : "",
		schedEverySecs: job.schedule.kind === "every" ? String(Math.round(job.schedule.everyMs / 1000)) : "",
		schedAtTimestamp: job.schedule.kind === "at" ? new Date(job.schedule.atMs).toISOString().slice(0, 16) : "",
	};
}

function agentTurnPayload(draft: CronModalDraft, message: string): CronPayload {
	const payload: CronPayload = { kind: "agentTurn", message, deliver: draft.deliverToChannel };
	if (draft.deliverToChannel && draft.deliverChannel) payload.channel = draft.deliverChannel;
	const recipient = draft.deliverTo.trim();
	if (draft.deliverToChannel && recipient) payload.to = recipient;
	if (draft.jobModel) payload.model = draft.jobModel;
	return payload;
}

function cronSaveCommand(draft: CronModalDraft): CronSaveCommand {
	const name = draft.jobName.trim();
	if (!name) return { ok: false, errorField: "name" };
	const parsedSchedule = parseScheduleFromForm(draft.schedKind, draft);
	if (!parsedSchedule.schedule) return { ok: false, errorField: parsedSchedule.error || "cron" };
	const message = draft.messageText.trim();
	if (!message) return { ok: false, errorField: "message" };
	const payload: CronPayload =
		draft.payloadKind === "systemEvent" ? { kind: "systemEvent", text: message } : agentTurnPayload(draft, message);
	return {
		ok: true,
		fields: {
			name,
			schedule: parsedSchedule.schedule,
			payload,
			sessionTarget: draft.sessionTarget,
			deleteAfterRun: draft.deleteAfterRun,
			enabled: draft.jobEnabled,
		},
	};
}

function CronModal(): VNode {
	const isEdit = !!editingJob.value;
	const job = editingJob.value;
	const saving = useSignal(false);
	const schedKind = useSignal("cron");
	const errorField = useSignal<string | null>(null);
	const jobModel = useSignal("");
	const jobName = useSignal("");
	const payloadKind = useSignal("systemEvent");
	const sessionTarget = useSignal("main");
	const messageText = useSignal("");
	const deleteAfterRun = useSignal(false);
	const jobEnabled = useSignal(true);
	const deliverToChannel = useSignal(false);
	const deliverChannel = useSignal("");
	const deliverTo = useSignal("");
	const schedCronExpr = useSignal("");
	const schedCronTz = useSignal("");
	const schedEverySecs = useSignal("");
	const schedAtTimestamp = useSignal("");

	useEffect(() => {
		const draft = cronModalDraft(editingJob.value);
		saving.value = false;
		errorField.value = null;
		schedKind.value = draft.schedKind;
		jobModel.value = draft.jobModel;
		jobName.value = draft.jobName;
		payloadKind.value = draft.payloadKind;
		sessionTarget.value = draft.sessionTarget;
		messageText.value = draft.messageText;
		deleteAfterRun.value = draft.deleteAfterRun;
		jobEnabled.value = draft.jobEnabled;
		deliverToChannel.value = draft.deliverToChannel;
		deliverChannel.value = draft.deliverChannel;
		deliverTo.value = draft.deliverTo;
		schedCronExpr.value = draft.schedCronExpr;
		schedCronTz.value = draft.schedCronTz;
		schedEverySecs.value = draft.schedEverySecs;
		schedAtTimestamp.value = draft.schedAtTimestamp;
	}, [editingJob.value]);

	function onSave(e: Event): void {
		e.preventDefault();
		const command = cronSaveCommand({
			schedKind: schedKind.value,
			jobModel: jobModel.value,
			jobName: jobName.value,
			payloadKind: payloadKind.value,
			sessionTarget: sessionTarget.value,
			messageText: messageText.value,
			deleteAfterRun: deleteAfterRun.value,
			jobEnabled: jobEnabled.value,
			deliverToChannel: deliverToChannel.value,
			deliverChannel: deliverChannel.value,
			deliverTo: deliverTo.value,
			schedCronExpr: schedCronExpr.value,
			schedCronTz: schedCronTz.value,
			schedEverySecs: schedEverySecs.value,
			schedAtTimestamp: schedAtTimestamp.value,
		});
		if (!command.ok) {
			errorField.value = command.errorField;
			return;
		}
		saving.value = true;
		const rpcMethod = isEdit ? "cron.update" : "cron.add";
		const rpcParams = isEdit ? { id: job?.id, patch: command.fields } : command.fields;
		sendRpc(rpcMethod, rpcParams).then((res) => {
			saving.value = false;
			if (res?.ok) {
				showModal.value = false;
				editingJob.value = null;
				loadJobs();
				loadStatus();
			}
		});
	}

	function schedParams(): VNode {
		if (schedKind.value === "at")
			return (
				<input
					data-field="at"
					className={`provider-key-input ${errorField.value === "at" ? "field-error" : ""}`}
					type="datetime-local"
					value={schedAtTimestamp.value}
					onInput={(e) => {
						schedAtTimestamp.value = (e.target as HTMLInputElement).value;
					}}
				/>
			);
		if (schedKind.value === "every")
			return (
				<input
					data-field="every"
					className={`provider-key-input ${errorField.value === "every" ? "field-error" : ""}`}
					type="number"
					placeholder="Interval in seconds"
					min="1"
					value={schedEverySecs.value}
					onInput={(e) => {
						schedEverySecs.value = (e.target as HTMLInputElement).value;
					}}
				/>
			);
		return (
			<>
				<input
					data-field="cron"
					className={`provider-key-input ${errorField.value === "cron" ? "field-error" : ""}`}
					placeholder="*/5 * * * *"
					value={schedCronExpr.value}
					onInput={(e) => {
						schedCronExpr.value = (e.target as HTMLInputElement).value;
					}}
				/>
				<input
					data-field="tz"
					className="provider-key-input"
					placeholder="Timezone (optional, e.g. Europe/Paris)"
					value={schedCronTz.value}
					onInput={(e) => {
						schedCronTz.value = (e.target as HTMLInputElement).value;
					}}
				/>
				<p className="text-xs text-[var(--muted)] mt-1">{cronTimezoneHelpText()}</p>
			</>
		);
	}

	return (
		<Modal
			show={showModal.value}
			onClose={() => {
				showModal.value = false;
				editingJob.value = null;
			}}
			title={isEdit ? "Edit Job" : "Add Job"}
		>
			<div className="provider-key-form">
				<label>
					<span className="text-xs text-[var(--muted)]">Name</span>
					<input
						data-field="name"
						className={`provider-key-input ${errorField.value === "name" ? "field-error" : ""}`}
						placeholder="Job name"
						value={jobName.value}
						onInput={(e) => {
							jobName.value = (e.target as HTMLInputElement).value;
						}}
					/>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Schedule Type</span>
					<select
						data-field="schedKind"
						className="provider-key-input"
						value={schedKind.value}
						onChange={(e) => {
							schedKind.value = (e.target as HTMLSelectElement).value;
						}}
					>
						<option value="at">Run Once</option>
						<option value="every">Every (interval)</option>
						<option value="cron">Cron (expression)</option>
					</select>
				</label>
				{schedParams()}
				<label>
					<span className="text-xs text-[var(--muted)]">Payload Type</span>
					<select
						data-field="payloadKind"
						className="provider-key-input"
						value={payloadKind.value}
						onChange={(e) => {
							payloadKind.value = (e.target as HTMLSelectElement).value;
							sessionTarget.value = (e.target as HTMLSelectElement).value === "systemEvent" ? "main" : "isolated";
						}}
					>
						<option value="systemEvent">System Event</option>
						<option value="agentTurn">Agent Turn</option>
					</select>
				</label>
				<p className="text-xs text-[var(--muted)] mt-1">
					{payloadKind.value === "agentTurn"
						? "Starts an isolated agent turn with this prompt. Enable channel delivery below to send the result to a chat."
						: "Adds this text to the main session as a system event when the job runs."}
				</p>
				<label>
					<span className="text-xs text-[var(--muted)]">Message</span>
					<textarea
						data-field="message"
						className={`provider-key-input textarea-sm ${errorField.value === "message" ? "field-error" : ""}`}
						placeholder={
							payloadKind.value === "agentTurn" ? "Prompt sent to the agent" : "Message sent to the main session"
						}
						value={messageText.value}
						onInput={(e) => {
							messageText.value = (e.target as HTMLTextAreaElement).value;
						}}
					/>
				</label>
				<span className="text-xs text-[var(--muted)]">Model (Agent Turn)</span>
				<ModelSelect
					ariaLabel="Model (Agent Turn)"
					models={modelsSig.value}
					value={jobModel.value}
					onChange={(v: string) => {
						jobModel.value = v;
					}}
					placeholder={defaultModelPlaceholder()}
				/>
				<p className="text-xs text-[var(--muted)] mt-1">Only used for Agent Turn jobs.</p>

				{payloadKind.value === "agentTurn" && (
					<div style={{ marginTop: "12px", borderTop: "1px solid var(--border)", paddingTop: "12px" }}>
						<label className="text-xs text-[var(--muted)] flex items-center gap-2">
							<input
								type="checkbox"
								checked={deliverToChannel.value}
								onChange={(e) => {
									deliverToChannel.value = (e.target as HTMLInputElement).checked;
								}}
							/>{" "}
							Deliver output to channel
						</label>
						{deliverToChannel.value && (
							<>
								<div className="mt-3">
									<span className="block text-xs text-[var(--muted)] mb-1">Channel Account</span>
									<ComboSelect
										ariaLabel="Channel Account"
										options={channelAccounts.value.map((c) => ({ value: c.account_id, label: c.name || c.account_id }))}
										value={deliverChannel.value}
										onChange={(v: string) => {
											deliverChannel.value = v;
										}}
										placeholder="Select channel account"
										searchPlaceholder="Search channels\u2026"
									/>
								</div>
								<div className="mt-3">
									<label>
										<span className="block text-xs text-[var(--muted)] mb-1">Chat ID (recipient)</span>
										<input
											className="provider-key-input"
											placeholder="Telegram chat_id"
											value={deliverTo.value}
											onInput={(e) => {
												deliverTo.value = (e.target as HTMLInputElement).value;
											}}
										/>
									</label>
								</div>
							</>
						)}
					</div>
				)}

				<label>
					<span className="text-xs text-[var(--muted)]">Session Target</span>
					<select
						data-field="target"
						className="provider-key-input"
						value={sessionTarget.value}
						onChange={(e) => {
							sessionTarget.value = (e.target as HTMLSelectElement).value;
							payloadKind.value = (e.target as HTMLSelectElement).value === "main" ? "systemEvent" : "agentTurn";
						}}
					>
						<option value="isolated">Isolated</option>
						<option value="main">Main</option>
					</select>
				</label>
				<label className="text-xs text-[var(--muted)] flex items-center gap-2">
					<input
						data-field="deleteAfter"
						type="checkbox"
						checked={deleteAfterRun.value}
						onChange={(e) => {
							deleteAfterRun.value = (e.target as HTMLInputElement).checked;
						}}
					/>{" "}
					Delete after run
				</label>
				<label className="text-xs text-[var(--muted)] flex items-center gap-2">
					<input
						data-field="enabled"
						type="checkbox"
						checked={jobEnabled.value}
						onChange={(e) => {
							jobEnabled.value = (e.target as HTMLInputElement).checked;
						}}
					/>{" "}
					Enabled
				</label>
				<div className="btn-row-mt">
					<button
						type="button"
						className="provider-btn provider-btn-secondary"
						onClick={() => {
							showModal.value = false;
							editingJob.value = null;
						}}
					>
						Cancel
					</button>
					<button type="button" className="provider-btn" onClick={onSave} disabled={saving.value}>
						{saving.value ? "Saving\u2026" : isEdit ? "Update" : "Create"}
					</button>
				</div>
			</div>
		</Modal>
	);
}

// ── Section panels ───────────────────────────────────────────

function HeartbeatPanel(): VNode {
	useEffect(() => {
		loadHeartbeatStatus();
		loadHeartbeatRuns();
	}, []);
	return (
		<div className="p-6">
			<HeartbeatSection />
		</div>
	);
}

function CronJobsPanel(): VNode {
	useEffect(() => {
		loadStatus();
		loadJobs();
		loadChannelAccounts();
	}, []);
	return (
		<div className="p-4 flex flex-col gap-4">
			<div className="flex items-center gap-3">
				<h2 className="text-lg font-medium text-[var(--text-strong)]">Cron Jobs</h2>
				<button
					type="button"
					className="provider-btn"
					onClick={() => {
						editingJob.value = null;
						showModal.value = true;
					}}
				>
					+ Add Job
				</button>
			</div>
			<StatusBar />
			<CronJobTable />
			<RunHistoryPanel />
		</div>
	);
}

// ── Main page ───────────────────────────────────────────────

function CronsPage(): VNode {
	return (
		<div className="flex-1 flex flex-col min-w-0 overflow-y-auto">
			{activeSection.value === "jobs" && <CronJobsPanel />}
			{activeSection.value === "heartbeat" && <HeartbeatPanel />}
			<CronModal />
			<ConfirmDialog />
		</div>
	);
}

export function initCrons(container: HTMLElement, param?: string | null): void {
	_cronsContainer = container;
	container.style.cssText = "padding:0;overflow:hidden;";
	cronJobs.value = (gon.get("crons") as CronJob[] | null) || [];
	cronStatus.value = gon.get("cron_status") as CronStatusInfo | null;
	heartbeatConfig.value = (gon.get("heartbeat_config") as HeartbeatConfig | null) || {};
	runsHistory.value = null;
	showModal.value = false;
	editingJob.value = null;
	heartbeatStatus.value = null;
	heartbeatRuns.value = (gon.get("heartbeat_runs") as CronRun[] | null) || [];
	channelAccounts.value = [];
	heartbeatModel.value = (gon.get("heartbeat_config") as HeartbeatConfig | null)?.model || "";
	activeSection.value = param === "heartbeat" ? "heartbeat" : "jobs";
	loadHeartbeatRuns();
	loadHeartbeatStatus();
	render(<CronsPage />, container);
}

export function teardownCrons(): void {
	if (_cronsContainer) render(null, _cronsContainer);
	_cronsContainer = null;
}
