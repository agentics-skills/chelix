// ── Channel step (channel selection, complex forms) ───────────
//
// ChannelStep orchestrates channel type selection and delegates to
// per-channel form components. Simpler forms (Telegram, Signal)
// live in channel-forms.tsx; complex forms below.

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { addChannel, parseChannelConfigPatch } from "../../channel-utils";
import { onEvent } from "../../events";
import { get as getGon } from "../../gon";
import { sendRpc } from "../../helpers";
import { t } from "../../i18n";
import { targetValue } from "../../typed-events";
import { WsEventName } from "../../types/ws-events";
import { ErrorPanel } from "../shared";
import type { ChannelFormProps } from "./channel-forms";
import {
	AdvancedConfigPatchField,
	ChannelStorageNotice,
	ChannelSuccess,
	ChannelTypeSelector,
	SignalForm,
	TelegramForm,
} from "./channel-forms";

interface WhatsAppStatusChannel {
	type: string;
	account_id: string;
	status: string;
	extra?: { qr_data?: string; qr_svg?: string };
}

interface WhatsAppPairingPollContext {
	accountId: string;
	hadQr: boolean;
	onConnected: () => void;
	onQr: (data: string, svg?: string) => void;
}

async function pollWhatsAppPairing(context: WhatsAppPairingPollContext): Promise<void> {
	try {
		const response = (await sendRpc("channels.status", {})) as {
			ok?: boolean;
			payload?: { channels?: WhatsAppStatusChannel[] };
		};
		if (!response?.ok) return;
		const channel = (response.payload?.channels || []).find(
			(candidate) => candidate.type === "whatsapp" && candidate.account_id === context.accountId,
		);
		if (!channel) return;
		if (channel.status === "connected" || (context.hadQr && !channel.extra?.qr_data)) {
			context.onConnected();
			return;
		}
		if (channel.extra?.qr_data) context.onQr(channel.extra.qr_data, channel.extra.qr_svg);
	} catch {
		// Polling retries transient status errors on the next interval.
	}
}

function WhatsAppForm({ onConnected, error, setError }: ChannelFormProps): VNode {
	const [accountId, setAccountId] = useState("");
	const [dmPolicy, setDmPolicy] = useState("allowlist");
	const [allowlist, setAllowlist] = useState("");
	const [advancedConfig, setAdvancedConfig] = useState("");
	const [saving, setSaving] = useState(false);
	const [pairingStarted, setPairingStarted] = useState(false);
	const [qrData, setQrData] = useState<string | null>(null);
	const [qrSvg, setQrSvg] = useState<string | null>(null);
	const [qrSvgUrl, setQrSvgUrl] = useState<string | null>(null);
	const [pairingError, setPairingError] = useState<string | null>(null);
	const unsubRef = useRef<(() => void) | null>(null);
	const hadQrRef = useRef(false);

	useEffect(() => {
		return () => {
			if (unsubRef.current) unsubRef.current();
		};
	}, []);

	useEffect(() => {
		if (!pairingStarted) return undefined;
		const id = accountId.trim() || "main";
		const timer = setInterval(() => {
			void pollWhatsAppPairing({
				accountId: id,
				hadQr: hadQrRef.current,
				onConnected: () => onConnected(id, "whatsapp"),
				onQr: (data, svg) => {
					hadQrRef.current = true;
					setQrData(data);
					if (svg) setQrSvg(svg);
				},
			});
		}, 2000);
		return () => clearInterval(timer);
	}, [pairingStarted]);

	useEffect(() => {
		if (!qrSvg) {
			setQrSvgUrl(null);
			return undefined;
		}
		let nextUrl: string | null = null;
		try {
			nextUrl = URL.createObjectURL(new Blob([qrSvg], { type: "image/svg+xml" }));
			setQrSvgUrl(nextUrl);
		} catch (_err) {
			setQrSvgUrl(null);
		}
		return () => {
			if (nextUrl) URL.revokeObjectURL(nextUrl);
		};
	}, [qrSvg]);

	function onStartPairing(e: Event): void {
		e.preventDefault();
		const id = accountId.trim() || "main";
		const advancedPatch = parseChannelConfigPatch(advancedConfig);
		if (!advancedPatch.ok) {
			setError(advancedPatch.error);
			return;
		}
		setError(null);
		setSaving(true);
		setQrData(null);
		setQrSvg(null);
		setPairingError(null);
		if (unsubRef.current) unsubRef.current();
		unsubRef.current = onEvent(WsEventName.Channel, (p) => {
			if (p.account_id !== id) return;
			if (p.kind === "pairing_qr_code") {
				setQrData(p.qr_data as string);
				setQrSvg((p.qr_svg as string) || null);
			}
			if (p.kind === "pairing_complete") onConnected(id, "whatsapp");
			if (p.kind === "pairing_failed") setPairingError((p.reason as string) || "Pairing failed");
		});
		const allowlistEntries = allowlist
			.trim()
			.split(/\n/)
			.map((s) => s.trim())
			.filter(Boolean);
		const config: Record<string, unknown> = { dm_policy: dmPolicy, allowlist: allowlistEntries };
		Object.assign(config, advancedPatch.value);
		(
			addChannel("whatsapp", id, config) as Promise<{ ok?: boolean; error?: { message?: string; detail?: string } }>
		).then((res) => {
			setSaving(false);
			if (res?.ok) {
				setPairingStarted(true);
			} else {
				if (unsubRef.current) {
					unsubRef.current();
					unsubRef.current = null;
				}
				setError((res?.error && (res.error.message || res.error.detail)) || "Failed to start pairing.");
			}
		});
	}

	if (pairingStarted) {
		return (
			<div className="flex flex-col gap-4 items-center">
				{pairingError ? (
					<ErrorPanel message={pairingError} />
				) : qrData ? (
					<div
						className="rounded-lg bg-white p-3"
						style="width:200px;height:200px;display:flex;align-items:center;justify-content:center;"
					>
						{qrSvgUrl ? (
							<img src={qrSvgUrl} alt="WhatsApp pairing QR code" style="width:100%;height:100%;display:block;" />
						) : (
							<div className="text-center text-xs text-gray-600">
								<div style="font-family:monospace;font-size:9px;word-break:break-all;max-height:180px;overflow:hidden;">
									{qrData.substring(0, 200)}
								</div>
							</div>
						)}
					</div>
				) : (
					<div className="text-sm text-[var(--muted)]">Waiting for QR code...</div>
				)}
				<div className="text-xs text-[var(--muted)] text-center">
					Scan the QR code from your terminal, or open WhatsApp &gt; Settings &gt; Linked Devices &gt; Link a Device.
				</div>
				<div className="text-xs text-[var(--muted)] text-center italic">
					Only new messages will be processed. Past conversations are not synced.
				</div>
			</div>
		);
	}

	return (
		<form onSubmit={onStartPairing} className="flex flex-col gap-3">
			<div className="rounded-md border border-[var(--border)] bg-[var(--surface2)] p-3 text-xs text-[var(--muted)] flex flex-col gap-1">
				<span className="font-medium text-[var(--text-strong)]">Link your WhatsApp</span>
				<span>1. Click "Start Pairing" to generate a QR code</span>
				<span>2. Open WhatsApp &gt; Settings &gt; Linked Devices &gt; Link a Device</span>
				<span>3. Scan the QR code to connect</span>
				<span className="mt-1 italic">
					Only new messages will be processed &mdash; past conversations are not synced.
				</span>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Account ID (optional)</span>
					<input
						type="text"
						className="provider-key-input w-full"
						value={accountId}
						onInput={(e) => setAccountId(targetValue(e))}
						placeholder="main"
						autoComplete="off"
						autoCapitalize="none"
						autoCorrect="off"
						spellcheck={false}
						name="whatsapp_account_id"
					/>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">DM Policy</span>
					<select
						className="provider-key-input w-full cursor-pointer"
						value={dmPolicy}
						onChange={(e) => setDmPolicy(targetValue(e))}
					>
						<option value="open">Open (anyone)</option>
						<option value="allowlist">Allowlist only</option>
						<option value="disabled">Disabled</option>
					</select>
				</label>
			</div>
			<div>
				<label>
					<span className="text-xs text-[var(--muted)] mb-1 block">Allowlist (optional)</span>
					<textarea
						className="provider-key-input w-full"
						rows={2}
						value={allowlist}
						onInput={(e) => setAllowlist(targetValue(e))}
						placeholder="phone number or identifier"
						style="resize:vertical;font-family:var(--font-body);"
					/>
				</label>
				<div className="text-xs text-[var(--muted)] mt-1">
					One per line. Only needed if DM policy is "Allowlist only".
				</div>
			</div>
			<AdvancedConfigPatchField value={advancedConfig} onInput={setAdvancedConfig} />
			{error && <ErrorPanel message={error} />}
			<button type="submit" className="provider-btn" disabled={saving}>
				{saving ? "Starting\u2026" : "Start Pairing"}
			</button>
		</form>
	);
}

// ── ChannelStep ─────────────────────────────────────────────

export function ChannelStep({ onNext, onBack }: { onNext: () => void; onBack: () => void }): VNode {
	const offeredList = (getGon("channels_offered") as string[] | null) || ["telegram", "whatsapp"];
	const offered = new Set(offeredList);
	const singleType = offeredList.length === 1 ? offeredList[0] : null;

	const [phase, setPhase] = useState<"select" | "form" | "success">(singleType ? "form" : "select");
	const [selectedType, setSelectedType] = useState<string | null>(singleType);
	const [connectedName, setConnectedName] = useState("");
	const [connectedType, setConnectedType] = useState<string | null>(null);
	const [channelError, setChannelError] = useState<string | null>(null);

	function onSelectType(type: string): void {
		setSelectedType(type);
		setPhase("form");
		setChannelError(null);
	}
	function onConnected(name: string, type: string): void {
		setConnectedName(name);
		setConnectedType(type);
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
			{phase === "form" && selectedType === "whatsapp" && (
				<WhatsAppForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "form" && selectedType === "signal" && (
				<SignalForm onConnected={onConnected} error={channelError} setError={setChannelError} />
			)}
			{phase === "success" && connectedType && (
				<ChannelSuccess channelName={connectedName} channelType={connectedType} onAnother={onAnother} />
			)}
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
