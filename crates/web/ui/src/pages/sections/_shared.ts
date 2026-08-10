// ── Shared state and helpers for settings sections ───────────

import { signal } from "@preact/signals";
import type { VNode } from "preact";
import { sendRpc } from "../../helpers";
import * as S from "../../state";

// ── Types ────────────────────────────────────────────────────

export interface UserLocationData {
	latitude: number;
	longitude: number;
	place?: string | null;
	updated_at?: number | null;
}

export interface UserProfileData {
	name?: string | null;
	timezone?: string | null;
	location?: UserLocationData | null;
}

import type { RpcResponse } from "../../types/rpc";
export type { RpcResponse };

export interface SectionNavigationItem {
	id: string;
	label: string;
	icon?: VNode;
	page?: boolean;
}

export interface SectionGroupHeading {
	group: string;
}

export type SectionItem = SectionNavigationItem | SectionGroupHeading;

// ── Module-level signals ─────────────────────────────────────

export const userProfile = signal<UserProfileData | null>(null);
export const loading = signal(true);
export const activeSection = signal("profile");
export const activeSubPath = signal("");
export const mobileSidebarVisible = signal(true);

// ── Mount state ──────────────────────────────────────────────

let _mounted = false;
let _containerRef: HTMLElement | null = null;

export function isMounted(): boolean {
	return _mounted;
}

export function setMounted(value: boolean): void {
	_mounted = value;
}

export function getContainerRef(): HTMLElement | null {
	return _containerRef;
}

export function setContainerRef(element: HTMLElement | null): void {
	_containerRef = element;
}

// ── Render helper ────────────────────────────────────────────

let _rerenderFn: (() => void) | null = null;

export function setRerenderFn(fn: () => void): void {
	_rerenderFn = fn;
}

export function rerender(): void {
	if (_rerenderFn) _rerenderFn();
}

// ── Utility helpers ──────────────────────────────────────────

export function isMobileViewport(): boolean {
	return window.innerWidth < 768;
}

export function isSafariBrowser(): boolean {
	if (typeof navigator === "undefined") return false;
	const ua = navigator.userAgent || "";
	const vendor = navigator.vendor || "";
	if (!ua.includes("Safari/")) return false;
	if (/(Chrome|CriOS|Chromium|Edg|OPR|FxiOS|Firefox|SamsungBrowser)/.test(ua)) return false;
	return /Apple/i.test(vendor) || ua.includes("Safari/");
}

export function fetchUserProfile(): void {
	if (!_mounted) return;
	sendRpc("user.get", {}).then((response) => {
		if (response.ok) {
			userProfile.value = (response.payload || {}) as UserProfileData;
			loading.value = false;
			rerender();
		} else if (_mounted && !S.connected) {
			setTimeout(fetchUserProfile, 500);
		} else {
			loading.value = false;
			rerender();
		}
	});
}
