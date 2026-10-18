// ── Channel step ─────────────────────────────────────────────
//
// ChannelStep selects a channel type and renders the Telegram form.

import type { VNode } from "preact";
import { useState } from "preact/hooks";
import { get as getGon } from "../../gon";
import { t } from "../../i18n";
import { ChannelStorageNotice, ChannelSuccess, ChannelTypeSelector, TelegramForm } from "./channel-forms";

export function ChannelStep({ onNext, onBack }: { onNext: () => void; onBack: () => void }): VNode {
	const offeredList = (getGon("channels_offered") as string[] | null) || ["telegram"];
	const offered = new Set(offeredList);
	const singleType = offeredList.length === 1 ? offeredList[0] : null;

	const [phase, setPhase] = useState<"select" | "form" | "success">(singleType ? "form" : "select");
	const [selectedType, setSelectedType] = useState<string | null>(singleType);
	const [connectedName, setConnectedName] = useState("");
	const [channelError, setChannelError] = useState<string | null>(null);

	function onSelectType(type: string): void {
		setSelectedType(type);
		setPhase("form");
		setChannelError(null);
	}
	function onConnected(name: string): void {
		setConnectedName(name);
		setPhase("success");
		setChannelError(null);
	}
	function onAnother(): void {
		if (singleType) {
			setPhase("form");
			setChannelError(null);
		} else {
			setPhase("select");
			setSelectedType(null);
			setChannelError(null);
		}
	}

	const showBackSelector = phase === "form" && !singleType;

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">Connect a Channel</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">
				Connect a messaging channel so you can chat from your phone or team workspace. You can set this up later in
				Channels.
			</p>
			<ChannelStorageNotice />
			{phase === "select" && <ChannelTypeSelector onSelect={onSelectType} offered={offered} />}
			{phase === "form" && selectedType === "telegram" && (
				<TelegramForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "success" && <ChannelSuccess channelName={connectedName} onAnother={onAnother} />}
			<div className="flex flex-wrap items-center gap-3 mt-1">
				<button
					type="button"
					className="provider-btn provider-btn-secondary"
					onClick={showBackSelector ? () => setPhase("select") : onBack}
				>
					{t("common:actions.back")}
				</button>
				{phase === "success" && (
					<button type="button" className="provider-btn" onClick={onNext}>
						{t("common:actions.continue")}
					</button>
				)}
				<button
					type="button"
					className="text-xs text-[var(--muted)] cursor-pointer bg-transparent border-none underline"
					onClick={onNext}
				>
					{t("common:actions.skip")}
				</button>
			</div>
		</div>
	);
}
