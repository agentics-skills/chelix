// ── Memory section ────────────────────────────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import {
	SaveButton,
	SectionHeading,
	StatusMessage,
	SubHeading,
	useSaveState,
} from "../../components/forms/SectionLayout";
import { sendRpc } from "../../helpers";
import { targetChecked, targetValue } from "../../typed-events";
import type { RpcResponse } from "./_shared";
import { rerender } from "./_shared";

interface MemoryStatus {
	total_files?: number;
	total_chunks?: number;
	embedding_model?: string;
	db_size_display?: string;
}

interface MemoryConfig {
	style?: string;
	agent_write_mode?: string;
	user_profile_write_mode?: string;
	provider?: string;
	citations?: string;
	llm_reranking?: boolean;
	search_merge_strategy?: string;
	session_export?: string;
	prompt_memory_mode?: string;
	enable_prefetch?: boolean;
	prefetch_limit?: number;
	auto_extract_interval?: number;
	enable_session_summary?: boolean;
	enable_self_improvement?: boolean;
}

function configString(value: string | undefined, fallback: string): string {
	return value || fallback;
}

function configBoolean(value: boolean | undefined, fallback: boolean): boolean {
	return value ?? fallback;
}

function configNumber(value: number | undefined, fallback: number): number {
	return value ?? fallback;
}

type MemorySettingDescription = string | VNode;
type MemorySelectOption = [value: string, label: string];

interface MemorySelectSettingProps {
	title: string;
	description: MemorySettingDescription;
	value: string;
	options: MemorySelectOption[];
	disabled?: boolean;
	disabledMessage?: string;
	onChange: (value: string) => void;
}

function MemorySelectSetting(props: MemorySelectSettingProps): VNode {
	return (
		<div>
			<SubHeading title={props.title} />
			<p className="text-xs text-[var(--muted)] mt-0 mb-2">{props.description}</p>
			<select
				className="provider-key-input min-w-[180px] w-auto"
				value={props.value}
				disabled={props.disabled}
				onChange={(event) => props.onChange(targetValue(event))}
			>
				{props.options.map(([value, label]) => (
					<option key={value} value={value}>
						{label}
					</option>
				))}
			</select>
			{props.disabled && props.disabledMessage && (
				<div className="text-xs text-[var(--muted)] mt-2">{props.disabledMessage}</div>
			)}
		</div>
	);
}

function MemoryCheckboxSetting({
	checked,
	title,
	description,
	onChange,
}: {
	checked: boolean;
	title: string;
	description: string;
	onChange: (checked: boolean) => void;
}): VNode {
	return (
		<label className="flex items-center gap-2 cursor-pointer">
			<input type="checkbox" checked={checked} onChange={(event) => onChange(targetChecked(event))} />
			<div>
				<span className="text-sm font-medium text-[var(--text-strong)]">{title}</span>
				<p className="text-xs text-[var(--muted)] mt-0.5 mb-0">{description}</p>
			</div>
		</label>
	);
}

function updateMemorySetting<T>(setter: (nextValue: T) => void, value: T): void {
	setter(value);
	rerender();
}

function MemoryStatusCard({ status }: { status: MemoryStatus | null }): VNode | null {
	if (!status) return null;
	const items = [
		{ label: "Files", value: status.total_files || 0 },
		{ label: "Chunks", value: status.total_chunks || 0 },
		{ label: "Model", value: status.embedding_model || "none", mono: true },
		{ label: "DB Size", value: status.db_size_display || "0 B" },
	];
	return (
		<div className="max-w-form py-3 px-4 rounded-md border border-[var(--border)] bg-[var(--bg)]">
			<SubHeading title="Status" />
			<div className="grid grid-cols-2 gap-y-2 gap-x-4 text-[.8rem]">
				{items.map((item) => (
					<div key={item.label}>
						<span className="text-[var(--muted)]">{item.label}:</span>
						<span className={`text-[var(--text)] ml-1.5 ${item.mono ? "font-mono text-xs" : ""}`}>{item.value}</span>
					</div>
				))}
			</div>
		</div>
	);
}

interface SelfImprovementPanelProps {
	enableSelfImprovement: boolean;
	enablePrefetch: boolean;
	prefetchLimit: number;
	autoExtractInterval: number;
	enableSessionSummary: boolean;
	onSelfImprovement: (enabled: boolean) => void;
	onPrefetch: (enabled: boolean) => void;
	onPrefetchLimit: (limit: number) => void;
	onAutoExtractInterval: (interval: number) => void;
	onSessionSummary: (enabled: boolean) => void;
}

function MemoryToggle({
	checked,
	title,
	description,
	onChange,
}: {
	checked: boolean;
	title: string;
	description: string;
	onChange: (checked: boolean) => void;
}): VNode {
	return (
		<label className="text-xs flex items-center gap-2 cursor-pointer">
			<input type="checkbox" checked={checked} onChange={(event) => onChange(targetChecked(event))} />
			<div>
				<span className="text-[var(--text)]">{title}</span>
				<span className="text-[var(--muted)] block text-[.7rem]">{description}</span>
			</div>
		</label>
	);
}

function SelfImprovementPanel(props: SelfImprovementPanelProps): VNode {
	return (
		<div>
			<SubHeading title="Agent Self-Improvement" />
			<p className="text-xs text-[var(--muted)] mt-0 mb-2">
				Controls how the agent learns autonomously across sessions.
			</p>
			<div className="flex flex-col gap-2.5">
				<MemoryToggle
					checked={props.enableSelfImprovement}
					title="Skill self-improvement prompting"
					description="Encourage the agent to create reusable skills after complex tasks"
					onChange={props.onSelfImprovement}
				/>
				<MemoryToggle
					checked={props.enablePrefetch}
					title="Memory recall (prefetch)"
					description="Automatically recall relevant memories before each turn"
					onChange={props.onPrefetch}
				/>
				{props.enablePrefetch && (
					<label className="ml-6 text-xs text-[var(--muted)]">
						Max results per turn:{" "}
						<input
							type="number"
							min={1}
							max={10}
							className="provider-key-input w-[60px] ml-1"
							value={props.prefetchLimit}
							onChange={(event) => props.onPrefetchLimit(Number.parseInt(targetValue(event), 10) || 3)}
						/>
					</label>
				)}
				<MemoryToggle
					checked={props.autoExtractInterval > 0}
					title="Periodic memory extraction"
					description="Automatically save important context every N turns"
					onChange={(enabled) => props.onAutoExtractInterval(enabled ? 5 : 0)}
				/>
				{props.autoExtractInterval > 0 && (
					<label className="ml-6 text-xs text-[var(--muted)]">
						Every{" "}
						<input
							type="number"
							min={1}
							max={50}
							className="provider-key-input w-[60px] mx-1"
							value={props.autoExtractInterval}
							onChange={(event) => props.onAutoExtractInterval(Number.parseInt(targetValue(event), 10) || 5)}
						/>{" "}
						turns
					</label>
				)}
				<MemoryToggle
					checked={props.enableSessionSummary}
					title="Session-end summary"
					description="Summarize accomplishments when a session is reset"
					onChange={props.onSessionSummary}
				/>
			</div>
		</div>
	);
}

export function MemorySection(): VNode {
	const [memStatus, setMemStatus] = useState<MemoryStatus | null>(null);
	const [memLoading, setMemLoading] = useState(true);
	const save = useSaveState();

	const [style, setStyle] = useState("hybrid");
	const [agentWriteMode, setAgentWriteMode] = useState("hybrid");
	const [userProfileWriteMode, setUserProfileWriteMode] = useState("explicit-and-auto");
	const [provider, setProvider] = useState("auto");
	const [citations, setCitations] = useState("auto");
	const [llmReranking, setLlmReranking] = useState(false);
	const [searchMergeStrategy, setSearchMergeStrategy] = useState("rrf");
	const [sessionExport, setSessionExport] = useState("on-new-or-reset");
	const [promptMemoryMode, setPromptMemoryMode] = useState("live-reload");
	const [enablePrefetch, setEnablePrefetch] = useState(true);
	const [prefetchLimit, setPrefetchLimit] = useState(3);
	const [autoExtractInterval, setAutoExtractInterval] = useState(5);
	const [enableSessionSummary, setEnableSessionSummary] = useState(true);
	const [enableSelfImprovement, setEnableSelfImprovement] = useState(true);

	function applyMemoryConfig(config: MemoryConfig): void {
		setStyle(configString(config.style, "hybrid"));
		setAgentWriteMode(configString(config.agent_write_mode, "hybrid"));
		setUserProfileWriteMode(configString(config.user_profile_write_mode, "explicit-and-auto"));
		setProvider(configString(config.provider, "auto"));
		setCitations(configString(config.citations, "auto"));
		setLlmReranking(configBoolean(config.llm_reranking, false));
		setSearchMergeStrategy(configString(config.search_merge_strategy, "rrf"));
		setSessionExport(configString(config.session_export, "on-new-or-reset"));
		setPromptMemoryMode(configString(config.prompt_memory_mode, "live-reload"));
		setEnablePrefetch(configBoolean(config.enable_prefetch, true));
		setPrefetchLimit(configNumber(config.prefetch_limit, 3));
		setAutoExtractInterval(configNumber(config.auto_extract_interval, 5));
		setEnableSessionSummary(configBoolean(config.enable_session_summary, true));
		setEnableSelfImprovement(configBoolean(config.enable_self_improvement, true));
	}

	function applyMemoryResponses(statusRes: RpcResponse, configRes: RpcResponse): void {
		if (statusRes?.ok) setMemStatus(statusRes.payload as MemoryStatus);
		if (configRes?.ok) applyMemoryConfig(configRes.payload as MemoryConfig);
		setMemLoading(false);
		rerender();
	}

	useEffect(() => {
		Promise.all([sendRpc("memory.status", {}), sendRpc("memory.config.get", {})])
			.then(([statusRes, configRes]: [RpcResponse, RpcResponse]) => {
				applyMemoryResponses(statusRes, configRes);
			})
			.catch(() => {
				setMemLoading(false);
				rerender();
			});
	}, []);

	function onSave(e: Event): void {
		e.preventDefault();
		save.setError(null);
		save.setSaving(true);

		sendRpc("memory.config.update", {
			style,
			agent_write_mode: agentWriteMode,
			user_profile_write_mode: userProfileWriteMode,
			provider,
			citations,
			llm_reranking: llmReranking,
			search_merge_strategy: searchMergeStrategy,
			session_export: sessionExport,
			prompt_memory_mode: promptMemoryMode,
			enable_prefetch: enablePrefetch,
			prefetch_limit: prefetchLimit,
			auto_extract_interval: autoExtractInterval,
			enable_session_summary: enableSessionSummary,
			enable_self_improvement: enableSelfImprovement,
		}).then((res: RpcResponse) => {
			save.setSaving(false);
			if (res?.ok) {
				save.flashSaved();
			} else {
				save.setError((res?.error as { message?: string })?.message || "Failed to save");
			}
			rerender();
		});
	}

	const setStyleAndRender = (value: string): void => updateMemorySetting(setStyle, value);
	const setPromptMemoryModeAndRender = (value: string): void => updateMemorySetting(setPromptMemoryMode, value);
	const setAgentWriteModeAndRender = (value: string): void => updateMemorySetting(setAgentWriteMode, value);
	const setUserProfileWriteModeAndRender = (value: string): void => updateMemorySetting(setUserProfileWriteMode, value);
	const setProviderAndRender = (value: string): void => updateMemorySetting(setProvider, value);
	const setCitationsAndRender = (value: string): void => updateMemorySetting(setCitations, value);
	const setSearchMergeStrategyAndRender = (value: string): void => updateMemorySetting(setSearchMergeStrategy, value);
	const setLlmRerankingAndRender = (value: boolean): void => updateMemorySetting(setLlmReranking, value);
	const setEnableSelfImprovementAndRender = (value: boolean): void =>
		updateMemorySetting(setEnableSelfImprovement, value);
	const setEnablePrefetchAndRender = (value: boolean): void => updateMemorySetting(setEnablePrefetch, value);
	const setPrefetchLimitAndRender = (value: number): void => updateMemorySetting(setPrefetchLimit, value);
	const setAutoExtractIntervalAndRender = (value: number): void => updateMemorySetting(setAutoExtractInterval, value);
	const setEnableSessionSummaryAndRender = (value: boolean): void =>
		updateMemorySetting(setEnableSessionSummary, value);
	const setSessionExportAndRender = (value: string): void => updateMemorySetting(setSessionExport, value);

	if (memLoading) {
		return (
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<SectionHeading title="Memory" />
				<div className="text-xs text-[var(--muted)]">Loading{"\u2026"}</div>
			</div>
		);
	}

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<SectionHeading title="Memory" />
			<p className="text-xs text-[var(--muted)] leading-relaxed max-w-form m-0">
				Configure how the agent stores and retrieves long-term memory. Memory enables the agent to recall past
				conversations, notes, and context across sessions.
			</p>
			<MemoryStatusCard status={memStatus} />
			<form onSubmit={onSave} className="max-w-form flex flex-col gap-4">
				<MemorySelectSetting
					title="Memory Style"
					description={
						<>
							Choose the high-level orchestration model. This controls whether prompt-visible <code>MEMORY.md</code> and
							memory tools are both active, one is active, or both are off.
						</>
					}
					value={style}
					options={[
						["hybrid", "Hybrid"],
						["prompt-only", "Prompt-only"],
						["search-only", "Search-only"],
						["off", "Off"],
					]}
					onChange={setStyleAndRender}
				/>
				<MemorySelectSetting
					title="Prompt Memory Mode"
					description={
						<>
							When prompt memory is enabled, choose whether <code>MEMORY.md</code> is reread on every turn or frozen
							when the session starts.
						</>
					}
					value={promptMemoryMode}
					options={[
						["live-reload", "Live reload"],
						["frozen-at-session-start", "Frozen at session start"],
					]}
					disabled={style === "search-only" || style === "off"}
					disabledMessage="Prompt memory is disabled by the current memory style, so this setting will only matter after you re-enable prompt memory."
					onChange={setPromptMemoryModeAndRender}
				/>
				<MemorySelectSetting
					title="Agent Memory Writes"
					description={
						<>
							Control where agent-authored memory writes can land. This affects <code>memory_save</code> and silent
							compaction memory flushes.
						</>
					}
					value={agentWriteMode}
					options={[
						["hybrid", "Hybrid (MEMORY.md and memory/*.md)"],
						["prompt-only", "Prompt-only (MEMORY.md only)"],
						["search-only", "Search-only (memory/*.md only)"],
						["off", "Off"],
					]}
					onChange={setAgentWriteModeAndRender}
				/>
				<MemorySelectSetting
					title="USER.md Writes"
					description={
						<>
							Control whether Chelix mirrors your profile into <code>USER.md</code>, and whether browser or channel
							timezone/location signals can update it silently.
						</>
					}
					value={userProfileWriteMode}
					options={[
						["explicit-and-auto", "Explicit and auto"],
						["explicit-only", "Explicit only"],
						["off", "Off (chelix.toml only)"],
					]}
					onChange={setUserProfileWriteModeAndRender}
				/>
				<MemorySelectSetting
					title="Embedding Provider"
					description="Select which embedding provider the memory system should use for RAG."
					value={provider}
					options={[
						["auto", "Auto-detect"],
						["local", "Local"],
						["openai", "OpenAI"],
						["custom", "Custom OpenAI-compatible"],
					]}
					onChange={setProviderAndRender}
				/>
				<MemorySelectSetting
					title="Citations"
					description="Include source file and line number with search results to help track where information comes from."
					value={citations}
					options={[
						["auto", "Auto (multi-file only)"],
						["on", "Always"],
						["off", "Never"],
					]}
					onChange={setCitationsAndRender}
				/>
				<MemorySelectSetting
					title="Search Merge Strategy"
					description="Choose how Chelix blends vector and keyword memory hits before optional reranking."
					value={searchMergeStrategy}
					options={[
						["rrf", "RRF"],
						["linear", "Linear"],
					]}
					onChange={setSearchMergeStrategyAndRender}
				/>
				<MemoryCheckboxSetting
					checked={llmReranking}
					title="LLM Reranking"
					description="Use the LLM to rerank search results for better relevance (slower but more accurate)."
					onChange={setLlmRerankingAndRender}
				/>
				<SelfImprovementPanel
					enableSelfImprovement={enableSelfImprovement}
					enablePrefetch={enablePrefetch}
					prefetchLimit={prefetchLimit}
					autoExtractInterval={autoExtractInterval}
					enableSessionSummary={enableSessionSummary}
					onSelfImprovement={setEnableSelfImprovementAndRender}
					onPrefetch={setEnablePrefetchAndRender}
					onPrefetchLimit={setPrefetchLimitAndRender}
					onAutoExtractInterval={setAutoExtractIntervalAndRender}
					onSessionSummary={setEnableSessionSummaryAndRender}
				/>
				<MemorySelectSetting
					title="Session Export"
					description="Export session transcripts into searchable memory when a session is rolled over."
					value={sessionExport}
					options={[
						["on-new-or-reset", "On session change"],
						["off", "Off"],
					]}
					onChange={setSessionExportAndRender}
				/>
				<div className="flex items-center gap-2 pt-2 border-t border-[var(--border)]">
					<SaveButton saving={save.saving} saved={save.saved} type="submit" />
					<StatusMessage error={save.error} success={save.saved ? "Saved" : null} />
				</div>
			</form>
		</div>
	);
}
