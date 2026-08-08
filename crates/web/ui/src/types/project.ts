// ── Project UI contracts ────────────────────────────────────

/** Project as returned by the projects.list RPC. */
export interface ProjectInfo {
	id: string;
	name?: string;
	description?: string;
	[key: string]: unknown;
}
