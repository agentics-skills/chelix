// ── Crons page English strings ───────────────────────────────

export default {
	// ── Sidebar ─────────────────────────────────────────────
	sidebar: {
		cronJobs: "Cron Jobs",
	},

	// ── Cron Jobs section ───────────────────────────────────
	jobs: {
		title: "Cron Jobs",
		addJob: "+ Add Job",
		noCronJobs: "No cron jobs configured.",
		// Status bar
		statusRunning: "Running",
		statusStopped: "Stopped",
		jobCount: "{{count}} job",
		jobCountPlural: "{{count}} jobs",
		enabledCount: "{{count}} enabled",
		nextRun: "next: {{time}}",
		// Table headers
		headerName: "Name",
		headerSchedule: "Schedule",
		headerNextRun: "Next Run",
		headerLastStatus: "Last Status",
		headerActions: "Actions",
		headerEnabled: "Enabled",
		// Actions
		edit: "Edit",
		run: "Run",
		history: "History",
		// Schedule formatting
		scheduleAt: "At {{time}}",
		scheduleEveryHours: "Every {{count}}h",
		scheduleEveryMinutes: "Every {{count}}m",
		scheduleEverySeconds: "Every {{count}}s",
		// Run history panel
		runHistoryTitle: "Run History: {{name}}",
		noRunsYet: "No runs yet.",
		// Delete confirmation
		deleteConfirm: "Delete job '{{name}}'?",
	},

	// ── Modal (Add / Edit) ──────────────────────────────────
	modal: {
		addJobTitle: "Add Job",
		editJobTitle: "Edit Job",
		nameLabel: "Name",
		namePlaceholder: "Job name",
		scheduleTypeLabel: "Schedule Type",
		scheduleAtOption: "At (one-shot)",
		scheduleEveryOption: "Every (interval)",
		scheduleCronOption: "Cron (expression)",
		everyPlaceholder: "Interval in seconds",
		cronPlaceholder: "*/5 * * * *",
		timezonePlaceholder: "Timezone (optional, e.g. Europe/Paris)",
		payloadTypeLabel: "Payload Type",
		systemEventOption: "System Event",
		agentTurnOption: "Agent Turn",
		messageLabel: "Message",
		messagePlaceholder: "Message text",
		sessionTargetLabel: "Session Target",
		targetIsolated: "Isolated",
		targetMain: "Main",
		deleteAfterRun: "Delete after run",
		create: "Create",
	},
};
