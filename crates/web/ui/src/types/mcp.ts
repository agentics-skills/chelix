// ── MCP UI contracts ────────────────────────────────────────

/** MCP server info as returned by the server. */
export interface McpServerInfo {
	name?: string;
	state?: string;
	[key: string]: unknown;
}
