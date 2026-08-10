// ── User Profile section ─────────────────────────────────────
//
// User-level settings: your name, UI language, version display.
// Agent configuration and prompts are managed on the Agents page.

import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { SectionHeading, StatusMessage, SubHeading } from "../../components/forms/SectionLayout";
import * as gon from "../../gon";
import { refresh as refreshGon } from "../../gon";
import { sendRpc } from "../../helpers";
import { setLocale } from "../../i18n";
import { targetValue } from "../../typed-events";
import type { RpcResponse, UserProfileData } from "./_shared";
import { loading, rerender, userProfile } from "./_shared";

export function IdentitySection(): VNode {
	const profile = userProfile.value;
	const storedLocale = localStorage.getItem("chelix-locale");

	const [userName, setUserName] = useState(profile?.name || "");
	const [uiLanguage, setUiLanguage] = useState(storedLocale || "auto");
	const [userNameSaving, setUserNameSaving] = useState(false);
	const [languageSaving, setLanguageSaving] = useState(false);
	const [saved, setSaved] = useState(false);
	const [languageSaved, setLanguageSaved] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [languageError, setLanguageError] = useState<string | null>(null);
	const userNameEditingRef = useRef(false);

	useEffect(() => {
		if (!profile) return;
		if (userNameEditingRef.current) return;
		setUserName(profile.name || "");
	}, [profile]);

	const savedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
	function flashSaved(): void {
		if (savedTimerRef.current) clearTimeout(savedTimerRef.current);
		setSaved(true);
		savedTimerRef.current = setTimeout(() => {
			savedTimerRef.current = null;
			setSaved(false);
			rerender();
		}, 2000);
	}

	if (loading.value) {
		return (
			<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
				<div className="text-xs text-[var(--muted)]">Loading{"\u2026"}</div>
			</div>
		);
	}

	function onUserNameBlur(e: Event): void {
		const trimmed = targetValue(e).trim();
		const currentValue = (userProfile.value?.name || "").trim();
		if (trimmed === currentValue) {
			userNameEditingRef.current = false;
			return;
		}
		if (!trimmed) {
			userNameEditingRef.current = false;
			setError("Your name is required.");
			return;
		}
		setError(null);
		setSaved(false);
		setUserNameSaving(true);
		const currentProfile = userProfile.value;
		if (!currentProfile) {
			setUserNameSaving(false);
			userNameEditingRef.current = false;
			setError("User profile is not loaded.");
			return;
		}
		sendRpc("user.update", {
			name: trimmed,
			timezone: currentProfile.timezone || null,
			location: currentProfile.location
				? {
						latitude: currentProfile.location.latitude,
						longitude: currentProfile.location.longitude,
						place: currentProfile.location.place || null,
					}
				: null,
		}).then((res: RpcResponse) => {
			setUserNameSaving(false);
			userNameEditingRef.current = false;
			if (res?.ok) {
				const payload = (res.payload || {}) as UserProfileData;
				userProfile.value = payload;
				refreshGon();
				setUserName(payload.name || "");
				flashSaved();
			} else {
				setError(res?.error?.message || "Failed to save");
			}
			rerender();
		});
	}

	function onApplyLanguage(): void {
		setLanguageSaving(true);
		setLanguageSaved(false);
		setLanguageError(null);

		const nextLanguage = uiLanguage === "auto" ? navigator.language || "en" : uiLanguage;
		setLocale(nextLanguage)
			.then(() => {
				if (uiLanguage === "auto") {
					localStorage.removeItem("chelix-locale");
				}
				setLanguageSaving(false);
				setLanguageSaved(true);
				setTimeout(() => {
					setLanguageSaved(false);
					rerender();
				}, 2000);
				rerender();
			})
			.catch((err: Error) => {
				setLanguageSaving(false);
				setLanguageError(err?.message || "Failed to update language");
				rerender();
			});
	}

	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<SectionHeading title="User Profile" />
			<div style={{ maxWidth: "600px", display: "flex", flexDirection: "column", gap: "16px" }}>
				{/* User name */}
				<div>
					<SubHeading title="Your Name" />
					<p className="text-xs text-[var(--muted)]" style={{ margin: "0 0 8px" }}>
						Saved to your user profile. Depending on memory settings, Chelix may also mirror it to <code>USER.md</code>.
					</p>
					<div className="flex items-center gap-2">
						<input
							type="text"
							className="provider-key-input"
							style={{ width: "100%", maxWidth: "280px" }}
							value={userName}
							onFocus={() => {
								userNameEditingRef.current = true;
							}}
							onInput={(e: Event) => setUserName(targetValue(e))}
							onBlur={onUserNameBlur}
							placeholder="e.g. Alice"
						/>
						{userNameSaving && <span className="text-xs text-[var(--muted)]">Saving{"\u2026"}</span>}
						<StatusMessage error={error} success={saved ? "Saved" : null} />
					</div>
				</div>

				{/* Language */}
				<div>
					<SubHeading title="Language" />
					<p className="text-xs text-[var(--muted)]" style={{ margin: "0 0 8px" }}>
						Choose the UI language for this browser.
					</p>
					<div style={{ display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap" }}>
						<label htmlFor="identityLanguageSelect" className="text-xs text-[var(--muted)]">
							UI language
						</label>
						<select
							id="identityLanguageSelect"
							className="provider-key-input"
							style={{ maxWidth: "220px" }}
							value={uiLanguage}
							onChange={(e: Event) => {
								setUiLanguage(targetValue(e));
								setLanguageSaved(false);
								setLanguageError(null);
								rerender();
							}}
						>
							<option value="auto">Browser default</option>
							<option value="en">English</option>
						</select>
						<button
							type="button"
							id="identityLanguageApplyBtn"
							className="provider-btn provider-btn-secondary"
							disabled={languageSaving}
							onClick={onApplyLanguage}
						>
							{languageSaving ? "Applying..." : "Apply language"}
						</button>
						<StatusMessage error={languageError} success={languageSaved ? "Language updated" : null} />
					</div>
				</div>
			</div>
			{gon.get("version") ? (
				<p className="text-xs text-[var(--muted)]" style={{ marginTop: "auto", paddingTop: "16px" }}>
					v{gon.get("version")}
				</p>
			) : null}
		</div>
	);
}
