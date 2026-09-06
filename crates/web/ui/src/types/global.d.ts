// ── Global window augmentations ─────────────────────────────
//
// Ambient declarations for custom properties attached to `window`
// across the chelix web UI. This file has no imports/exports so
// its declarations are visible to every compilation unit, including
// standalone entry points like login-app.tsx.

interface Window {
	/** Server-injected data (gon pattern). See gon.ts for typed access. */
	__CHELIX__?: Partial<import("./gon").GonData>;
	/** Suppress the next password-changed WebSocket redirect. */
	__chelixSuppressNextPasswordChangedRedirect?: boolean;
}
