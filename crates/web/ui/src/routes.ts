// ── Central route definitions ────────────────────────────────
//
// All SPA paths are defined once in Rust (SpaRoutes) and injected
// via gon. This module re-exports them so JS never hardcodes paths.

import * as gon from "./gon";
import type { SpaRoutes } from "./types/gon";

const injectedRoutes = gon.get("routes");
if (!injectedRoutes) throw new Error("Missing required server-injected SPA routes.");

export const routes: SpaRoutes = injectedRoutes;

export function settingsPath(id: string): string {
	return `${routes.settings}/${id}`;
}
