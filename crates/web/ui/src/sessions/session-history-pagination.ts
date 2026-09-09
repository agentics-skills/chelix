import * as S from "../state";
import { applyHistoryPage, getHistoryWindow, getSessionHistory } from "../stores/session-history-cache";
import type { UiSnapshot } from "../types/ui-history";
import { showToast } from "../ui";
import { subscribeHistoryWindow } from "./history-subscription";
import { fetchSessionHistoryViaHttp, SESSION_HISTORY_PAGE_LIMIT } from "./session-history";
import { reconcileSessionHistory, renderHistory, type SearchContext } from "./session-render";

const HISTORY_HEADROOM_PX = 1200;
type Direction = "older" | "newer";
type HistoryWindow = NonNullable<ReturnType<typeof getHistoryWindow>>;
let scrollElement: HTMLElement | null = null;
let scrollRaf = 0;
let request = 0;
let filling = "";

function edgePosition(window: HistoryWindow, direction: Direction): number | undefined {
	return direction === "older" ? window.history[0]?.position : window.history.at(-1)?.position;
}

function needsPage(window: HistoryWindow, box: HTMLElement, direction: Direction): boolean {
	return direction === "older"
		? window.hasOlder && box.scrollTop < HISTORY_HEADROOM_PX
		: window.hasNewer && box.scrollHeight - box.scrollTop - box.clientHeight < HISTORY_HEADROOM_PX;
}

function nextDirection(window: HistoryWindow, box: HTMLElement): Direction | undefined {
	if (needsPage(window, box, "older")) return "older";
	if (needsPage(window, box, "newer")) return "newer";
	return undefined;
}

function isCurrentRequest(key: string, token: number): boolean {
	return key === S.activeSessionKey && token === request;
}

async function loadPage(key: string, token: number, window: HistoryWindow, direction: Direction): Promise<boolean> {
	const position = edgePosition(window, direction);
	if (position === undefined) return false;
	const page = await fetchSessionHistoryViaHttp(key, {
		...(direction === "older" ? { before: position } : { after: position }),
		generation: window.generation,
		limit: SESSION_HISTORY_PAGE_LIMIT,
	});
	if (!isCurrentRequest(key, token) || getHistoryWindow(key)?.generation !== window.generation) return false;
	if (!applyHistoryPage(key, page, direction)) return false;
	reconcileSessionHistory(key);
	await subscribeHistoryWindow(key);
	if (!isCurrentRequest(key, token)) return false;
	const current = getHistoryWindow(key);
	return !!current && edgePosition(current, direction) !== position;
}

async function fillPages(key: string, token: number): Promise<void> {
	let direction: Direction | undefined;
	while (isCurrentRequest(key, token) && S.chatMsgBox) {
		const window = getHistoryWindow(key);
		if (!window) return;
		direction ||= nextDirection(window, S.chatMsgBox);
		if (!(direction && needsPage(window, S.chatMsgBox, direction))) return;
		if (!(await loadPage(key, token, window, direction))) return;
	}
}

async function fillHistoryHeadroom(): Promise<void> {
	const key = S.activeSessionKey;
	if (filling === key) return;
	const token = request;
	filling = key;
	try {
		await fillPages(key, token);
	} catch (error) {
		if (isCurrentRequest(key, token))
			showToast(error instanceof Error ? error.message : "History loading failed", "error");
	} finally {
		if (isCurrentRequest(key, token)) filling = "";
	}
}

function handleScroll(): void {
	if (scrollRaf) return;
	scrollRaf = requestAnimationFrame(() => {
		scrollRaf = 0;
		void fillHistoryHeadroom();
	});
}

export function renderSessionHistory(
	key: string,
	history: UiSnapshot[],
	searchContext: SearchContext | null,
	totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	request += 1;
	filling = "";
	if (scrollElement !== S.chatMsgBox) {
		scrollElement?.removeEventListener("scroll", handleScroll);
		scrollElement = S.chatMsgBox;
		scrollElement?.addEventListener("scroll", handleScroll, { passive: true });
	}
	renderHistory(key, getSessionHistory(key) || history, searchContext, totalCountHint, skipAutoScroll);
	void fillHistoryHeadroom();
}
