// ── Skills page English strings ──────────────────────────────

export default {
	// ── Page header ─────────────────────────────────────────
	title: "Skills",
	refresh: "Refresh",
	description: "SKILL.md-based skills discovered from project, personal, and bundled paths.",
	howToWriteSkill: "How to write a skill?",

	// ── Connection ──────────────────────────────────────────
	notConnected: "Not connected to gateway.",
	failedGeneric: "Failed: {{error}}",

	// ── Loading ─────────────────────────────────────────────
	loadingSkills: "Loading skills\u2026",

	// ── Enabled skills table ────────────────────────────────
	enabledTitle: "Enabled Skills",
	colName: "Name",
	colDescription: "Description",
	colSource: "Source",
	deletedSkill: "Deleted {{name}}",
	disabledSkill: "Disabled {{name}}",
	cannotDisableUnknownSource: "Cannot disable: unknown source for skill.",
	deleteSkillConfirm: 'Delete skill "{{name}}"? This removes the entire skill directory.',
	disabling: "Disabling...",
	deleting: "Deleting...",

	// ── Skill detail panel ──────────────────────────────────
	protected: "Protected",
	protectedCannotDelete: "Skill {{name}} is protected and cannot be deleted from UI",
	failedToLoad: "Failed to load: {{error}}",
	skillMdSource: "SKILL.md source",

	// ── Badges ──────────────────────────────────────────────
	enabled: "enabled",

	// ── Security warning ────────────────────────────────────
	securityTitle: "\u26a0\ufe0f Skills can direct command execution \u2014 treat every skill as untrusted",
	securityIntro:
		"Skills are community-authored instructions that the AI agent follows under Chelix's global sandbox policy. Popularity or download count does not mean a skill is safe. A malicious skill can instruct the agent to:",
	threat1: "Execute arbitrary shell commands in the configured runtime (install malware, cryptominers, backdoors)",
	threat2:
		"Read and exfiltrate sensitive data exposed to the configured runtime \u2014 API tokens, credentials, env variables, and shared data",
	threat3: "Modify or delete files available to the configured runtime, including the shared Chelix data folder",
	threat4: "Send your data to remote servers via curl/wget without your knowledge",
	securityReview:
		"Triple-check the source code of every skill before enabling it. Read the full SKILL.md and any scripts it references \u2014 these are the exact instructions the agent will execute on your behalf. Do not trust a skill just because it is popular, highly downloaded, or appears on a leaderboard.",
	securitySandbox:
		'Global sandbox mode "On" runs command execution in a filesystem-isolated runtime. Mode "Off" executes commands directly on the host.',
	dismiss: "Dismiss",

	// ── Bundled categories ──────────────────────────────────
	bundledTitle: "Bundled Skill Categories",
	bundledDescription: "Toggle categories of built-in skills. Disabled categories are excluded from the agent context.",
};
