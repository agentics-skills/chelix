import type { SearchContext } from "./session-render";

let pending: { key: string; context: SearchContext } | null = null;

export function setSearchNavigation(key: string, context: SearchContext): void {
	pending = { key, context };
}

export function takeSearchNavigation(key: string): SearchContext | null {
	const current = pending;
	pending = null;
	return current?.key === key ? current.context : null;
}
