import type { ResolvedIdentity } from "./types/gon";

const DYNAMIC_FAVICON_SELECTOR = 'link[data-chelix-dynamic-favicon="true"]';

function trimString(value: unknown): string {
	return typeof value === "string" ? value.trim() : "";
}

export function identityName(identity: Partial<ResolvedIdentity> | null | undefined): string {
	const name = trimString(identity?.name);
	return name || "chelix";
}

export function identityEmoji(identity: Partial<ResolvedIdentity> | null | undefined): string {
	return trimString(identity?.emoji);
}

export function identityUserName(identity: Partial<ResolvedIdentity> | null | undefined): string {
	return trimString(identity?.user_name);
}

export function formatPageTitle(identity: Partial<ResolvedIdentity> | null | undefined): string {
	return identityName(identity);
}

export function formatLoginTitle(identity: Partial<ResolvedIdentity> | null | undefined): string {
	return identityName(identity);
}

function emojiFaviconPng(emoji: string): string | null {
	const canvas = document.createElement("canvas");
	canvas.width = 64;
	canvas.height = 64;
	const ctx = canvas.getContext("2d");
	if (!ctx) return null;
	ctx.clearRect(0, 0, 64, 64);
	ctx.textAlign = "center";
	ctx.textBaseline = "middle";
	ctx.font = "52px 'Apple Color Emoji','Segoe UI Emoji','Noto Color Emoji',sans-serif";
	ctx.fillText(emoji, 32, 34);
	return canvas.toDataURL("image/png");
}

function findDynamicFaviconLink(): HTMLLinkElement | null {
	return document.querySelector<HTMLLinkElement>(DYNAMIC_FAVICON_SELECTOR);
}

function ensureDynamicFaviconLink(): HTMLLinkElement {
	const existing = findDynamicFaviconLink();
	if (existing) return existing;

	const link = document.createElement("link");
	link.rel = "icon";
	link.dataset.chelixDynamicFavicon = "true";
	document.head.appendChild(link);
	return link;
}

export function applyIdentityFavicon(identity: Partial<ResolvedIdentity> | null | undefined): boolean {
	const emoji = identityEmoji(identity);
	if (!emoji) {
		findDynamicFaviconLink()?.remove();
		return false;
	}

	const href = emojiFaviconPng(emoji);
	if (!href) {
		findDynamicFaviconLink()?.remove();
		return false;
	}

	const link = ensureDynamicFaviconLink();
	link.type = "image/png";
	link.sizes = "64x64";
	link.href = href;
	return true;
}
