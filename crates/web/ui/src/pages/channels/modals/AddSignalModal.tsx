// ── Add Signal modal ─────────────────────────────────────────

import { useSignal } from "@preact/signals";
import type { VNode } from "preact";

import { addChannel, deriveSignalAccountId, parseChannelConfigPatch } from "../../../channel-utils";
import { models as modelsSig } from "../../../stores/model-store";
import { targetValue } from "../../../typed-events";
import { ChannelType } from "../../../types/channel";
import { Modal, ModelSelect } from "../../../ui";
import { type ChannelConfig, ConnectionModeHint, loadChannels, showAddSignal } from "../../ChannelsPage";
import { AdvancedConfigPatchField, AllowlistInput } from "../ChannelFields";

export function AddSignalModal(): VNode {
	const error = useSignal("");
	const saving = useSignal(false);
	const addModel = useSignal("");
	const allowlistItems = useSignal<string[]>([]);
	const groupAllowlistItems = useSignal<string[]>([]);
	const accountDraft = useSignal("");
	const httpUrlDraft = useSignal("http://127.0.0.1:8080");
	const dmPolicy = useSignal("allowlist");
	const groupPolicy = useSignal("disabled");
	const mentionMode = useSignal("mention");
	const advancedConfigPatch = useSignal("");

	function reset(): void {
		addModel.value = "";
		allowlistItems.value = [];
		groupAllowlistItems.value = [];
		accountDraft.value = "";
		httpUrlDraft.value = "http://127.0.0.1:8080";
		dmPolicy.value = "allowlist";
		groupPolicy.value = "disabled";
		mentionMode.value = "mention";
		advancedConfigPatch.value = "";
		error.value = "";
	}

	function onSubmit(e: Event): void {
		e.preventDefault();
		const account = accountDraft.value.trim();
		const httpUrl = httpUrlDraft.value.trim();
		if (!account) {
			error.value = "Signal account (phone number) is required.";
			return;
		}
		if (!httpUrl) {
			error.value = "signal-cli daemon URL is required.";
			return;
		}
		const advancedPatch = parseChannelConfigPatch(advancedConfigPatch.value);
		if (!advancedPatch.ok) {
			error.value = advancedPatch.error;
			return;
		}
		error.value = "";
		saving.value = true;
		const addConfig: ChannelConfig = {
			http_url: httpUrl,
			dm_policy: dmPolicy.value,
			allowlist: allowlistItems.value,
			group_policy: groupPolicy.value,
			group_allowlist: groupAllowlistItems.value,
			mention_mode: mentionMode.value,
		};
		addConfig.account = account;
		const accountId = deriveSignalAccountId(account);
		if (addModel.value) {
			addConfig.model = addModel.value;
			const found = modelsSig.value.find((x) => x.id === addModel.value);
			if (found?.provider) addConfig.model_provider = found.provider;
		}
		Object.assign(addConfig, advancedPatch.value);
		addChannel(ChannelType.Signal, accountId, addConfig).then((res: unknown) => {
			saving.value = false;
			const r = res as { ok?: boolean; error?: { message?: string; detail?: string } } | undefined;
			if (r?.ok) {
				showAddSignal.value = false;
				reset();
				loadChannels();
			} else {
				error.value = r?.error?.message || r?.error?.detail || "Failed to connect channel.";
			}
		});
	}

	return (
		<Modal
			show={showAddSignal.value}
			onClose={() => {
				showAddSignal.value = false;
			}}
			title="Connect Signal"
		>
			<div className="channel-form">
				<div className="channel-card">
					<div>
						<span className="text-xs font-medium text-[var(--text-strong)]">Requires signal-cli</span>
						<div className="text-xs text-[var(--muted)] channel-help">
							Signal integration requires a running{" "}
							<a
								href="https://github.com/AsamK/signal-cli"
								target="_blank"
								rel="noopener noreferrer"
								className="underline text-[var(--text-strong)]"
							>
								signal-cli
							</a>{" "}
							daemon with JSON-RPC HTTP enabled. Install it, register or link your Signal account, then start the
							daemon:
						</div>
						<code className="text-[10px] bg-[var(--surface1)] px-1.5 py-0.5 rounded mt-1 block">
							signal-cli daemon --http localhost:8080
						</code>
					</div>
				</div>
				<ConnectionModeHint type={ChannelType.Signal} />
				<label>
					<span className="text-xs text-[var(--muted)]">Signal Account (phone number)</span>
					<input
						data-field="account"
						type="text"
						placeholder="e.g. +15551234567"
						value={accountDraft.value}
						onInput={(e) => {
							accountDraft.value = targetValue(e);
						}}
						className="channel-input"
						autoComplete="off"
					/>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">signal-cli Daemon URL</span>
					<input
						data-field="httpUrl"
						type="url"
						placeholder="http://127.0.0.1:8080"
						value={httpUrlDraft.value}
						onInput={(e) => {
							httpUrlDraft.value = targetValue(e);
						}}
						className="channel-input"
					/>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">DM Policy</span>
					<select
						data-field="dmPolicy"
						className="channel-select"
						value={dmPolicy.value}
						onChange={(e) => {
							dmPolicy.value = targetValue(e);
						}}
					>
						<option value="allowlist">Allowlist only</option>
						<option value="open">Open (anyone)</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Group Policy</span>
					<select
						data-field="groupPolicy"
						className="channel-select"
						value={groupPolicy.value}
						onChange={(e) => {
							groupPolicy.value = targetValue(e);
						}}
					>
						<option value="disabled">Disabled</option>
						<option value="allowlist">Allowlist only</option>
						<option value="open">Open (any group)</option>
					</select>
				</label>
				<label>
					<span className="text-xs text-[var(--muted)]">Group Mention Mode</span>
					<select
						data-field="mentionMode"
						className="channel-select"
						value={mentionMode.value}
						onChange={(e) => {
							mentionMode.value = targetValue(e);
						}}
					>
						<option value="mention">Must mention bot</option>
						<option value="always">Always respond</option>
						<option value="none">Do not respond in groups</option>
					</select>
				</label>
				<span className="text-xs text-[var(--muted)]">Default Model</span>
				<ModelSelect
					ariaLabel="Default Model"
					models={modelsSig.value}
					value={addModel.value}
					onChange={(v: string) => {
						addModel.value = v;
					}}
				/>
				<span className="text-xs text-[var(--muted)]">DM Allowlist</span>
				<AllowlistInput
					ariaLabel="DM Allowlist"
					value={allowlistItems.value}
					onChange={(v) => {
						allowlistItems.value = v;
					}}
				/>
				<span className="text-xs text-[var(--muted)]">Group Allowlist</span>
				<AllowlistInput
					ariaLabel="Group Allowlist"
					value={groupAllowlistItems.value}
					onChange={(v) => {
						groupAllowlistItems.value = v;
					}}
				/>
				<AdvancedConfigPatchField
					value={advancedConfigPatch.value}
					onInput={(value) => {
						advancedConfigPatch.value = value;
					}}
				/>
				{error.value && <div className="text-xs text-[var(--error)] py-1">{error.value}</div>}
				<button type="button" className="provider-btn" onClick={onSubmit} disabled={saving.value}>
					{saving.value ? "Connecting\u2026" : "Connect Signal"}
				</button>
			</div>
		</Modal>
	);
}
