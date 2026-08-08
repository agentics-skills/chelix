// ── Identity step (name, timezone) ───────────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { EmojiPicker } from "../../emoji-picker";
import { get as getGon, refresh as refreshGon } from "../../gon";
import { t } from "../../i18n";
import { updateIdentity, validateIdentityFields } from "../../identity-utils";
import { targetValue } from "../../typed-events";
import { detectBrowserTimezone, ErrorPanel } from "../shared";
import type { IdentityInfo } from "../types";

type StringStateSetter = (value: string | ((previous: string) => string)) => void;

function applyRefreshedValue(
	value: string | undefined,
	setter: StringStateSetter,
	merge: (previous: string, refreshed: string) => string,
): void {
	if (value) setter((previous) => merge(previous, value));
}

function hydrateIdentityState(
	refreshed: IdentityInfo,
	setUserName: StringStateSetter,
	setName: StringStateSetter,
	setEmoji: StringStateSetter,
	setTheme: StringStateSetter,
): void {
	applyRefreshedValue(refreshed.user_name, setUserName, (previous, value) => previous || value);
	applyRefreshedValue(refreshed.name, setName, (previous, value) =>
		previous && previous !== "Chelix" ? previous : value,
	);
	applyRefreshedValue(refreshed.emoji, setEmoji, (previous, value) =>
		previous && previous !== "\u{1f916}" ? previous : value,
	);
	applyRefreshedValue(refreshed.theme, setTheme, (previous, value) => previous || value);
}

export function IdentityStep({ onNext, onBack }: { onNext: () => void; onBack?: (() => void) | null }): VNode {
	const identityData = (getGon("identity") as IdentityInfo) || {};
	const [userName, setUserName] = useState(identityData.user_name || "");
	const [name, setName] = useState(identityData.name || "Chelix");
	const [emoji, setEmoji] = useState(identityData.emoji || "\u{1f916}");
	const [theme, setTheme] = useState(identityData.theme || "");
	const [saving, setSaving] = useState(false);
	const [error, setError] = useState<string | null>(null);

	useEffect(() => {
		let cancelled = false;
		refreshGon().then(() => {
			if (cancelled) return;
			const refreshed = (getGon("identity") as IdentityInfo) || {};
			hydrateIdentityState(refreshed, setUserName, setName, setEmoji, setTheme);
		});
		return () => {
			cancelled = true;
		};
	}, []);

	function onSubmit(e: Event): void {
		e.preventDefault();
		const v = validateIdentityFields(name, userName);
		if (!v.valid) {
			setError(v.error);
			return;
		}
		setError(null);
		setSaving(true);
		const userTimezone = detectBrowserTimezone();
		updateIdentity({
			name: name.trim(),
			emoji: emoji.trim() || "",
			theme: theme.trim() || "",
			user_name: userName.trim(),
			user_timezone: userTimezone || "",
		}).then((res: { ok?: boolean; error?: { message?: string } } | null) => {
			setSaving(false);
			if (res?.ok) {
				refreshGon();
				onNext();
			} else {
				setError(res?.error?.message || "Failed to save");
			}
		});
	}

	return (
		<div className="flex flex-col gap-4">
			<h2 className="text-lg font-medium text-[var(--text-strong)]">{t("onboarding:identity.title")}</h2>
			<p className="text-xs text-[var(--muted)] leading-relaxed">Tell us about yourself and customise your agent.</p>
			<form onSubmit={onSubmit} className="flex flex-col gap-4">
				<div>
					<div className="text-xs text-[var(--muted)] mb-1">Your name *</div>
					<input
						type="text"
						className="provider-key-input w-full"
						value={userName}
						onInput={(e) => setUserName(targetValue(e))}
						placeholder="e.g. Alice"
						autofocus
					/>
				</div>
				<div className="flex flex-col gap-3">
					<div className="grid grid-cols-1 gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:gap-x-4">
						<div className="min-w-0">
							<div className="text-xs text-[var(--muted)] mb-1">Agent name *</div>
							<input
								type="text"
								className="provider-key-input w-full"
								value={name}
								onInput={(e) => setName(targetValue(e))}
								placeholder="e.g. Rex"
							/>
						</div>
						<div>
							<div className="text-xs text-[var(--muted)] mb-1">Emoji</div>
							<EmojiPicker value={emoji} onChange={setEmoji} />
						</div>
					</div>
					<div>
						<div className="text-xs text-[var(--muted)] mb-1">Theme</div>
						<input
							type="text"
							className="provider-key-input w-full"
							value={theme}
							onInput={(e) => setTheme(targetValue(e))}
							placeholder="wise owl, chill fox, witty robot{'\u2026'}"
						/>
					</div>
				</div>
				{error && <ErrorPanel message={error} />}
				<div className="flex flex-wrap items-center gap-3 mt-1">
					{onBack ? (
						<button type="button" className="provider-btn provider-btn-secondary" onClick={onBack}>
							{t("common:actions.back")}
						</button>
					) : null}
					<button key={`id-${saving}`} type="submit" className="provider-btn" disabled={saving}>
						{saving ? "Saving\u2026" : "Continue"}
					</button>
				</div>
			</form>
		</div>
	);
}
