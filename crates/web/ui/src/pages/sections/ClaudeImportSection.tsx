// ── Claude Code / Desktop Import section ──────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { SectionHeading } from "../../components/forms/SectionLayout";
import { sendRpc } from "../../helpers";
import type { RpcResponse } from "./_shared";
import { rerender } from "./_shared";
import { ImportCategoryGrid, type ImportResultData, ImportResultPanel } from "./ImportSectionComponents";

interface ClaudeScanResult {
	detected?: boolean;
	has_mcp_servers?: boolean;
	has_desktop_config?: boolean;
	skills_count?: number;
	commands_count?: number;
	has_memory?: boolean;
}

type ImportResult = ImportResultData;

interface ClaudeSelection {
	mcp_servers: boolean;
	skills: boolean;
	memory: boolean;
	[key: string]: boolean;
}

const CATEGORY_ICONS: Record<string, string> = {
	mcp_servers: "\uD83D\uDD17",
	skills: "\u2728",
	memory: "\uD83E\uDDE0",
};

export function ClaudeImportSection(): VNode {
	const [loading, setLoading] = useState(true);
	const [scan, setScan] = useState<ClaudeScanResult | null>(null);
	const [importing, setImporting] = useState(false);
	const [done, setDone] = useState(false);
	const [result, setResult] = useState<ImportResult | null>(null);
	const [error, setError] = useState<string | null>(null);
	const [selection, setSelection] = useState<ClaudeSelection>({
		mcp_servers: true,
		skills: true,
		memory: true,
	});

	useEffect(() => {
		let cancelled = false;
		sendRpc("claude.detect", {}).then((res: RpcResponse) => {
			if (cancelled) return;
			if (res?.ok) setScan(res.payload as ClaudeScanResult);
			else setError("Failed to scan Claude installation");
			setLoading(false);
			rerender();
		});
		return () => {
			cancelled = true;
		};
	}, []);

	function toggleCategory(key: string): void {
		setSelection((prev) => {
			const next = Object.assign({}, prev);
			next[key] = !prev[key];
			return next;
		});
	}

	function doImport(): void {
		setImporting(true);
		setError(null);
		sendRpc("claude.import", selection).then((res: RpcResponse) => {
			setImporting(false);
			if (res?.ok) {
				setResult(res.payload as ImportResult);
				setDone(true);
			} else {
				setError((res?.error as { message?: string })?.message || "Import failed");
			}
			rerender();
		});
	}

	function resetImport(): void {
		setDone(false);
		setResult(null);
		rerender();
	}

	if (loading) {
		return (
			<div>
				<SectionHeading title="Claude Code" />
				<div className="text-xs text-[var(--muted)]">Scanning{"\u2026"}</div>
			</div>
		);
	}

	if (!scan?.detected) {
		return (
			<div>
				<SectionHeading title="Claude Code" />
				<div className="text-xs text-[var(--muted)]">No Claude Code installation detected.</div>
			</div>
		);
	}

	const categories = [
		{
			key: "mcp_servers",
			label: "MCP Servers",
			available: scan.has_mcp_servers,
			detail: scan.has_desktop_config ? "from Claude Code + Desktop" : "from Claude Code",
		},
		{
			key: "skills",
			label: "Skills",
			available: (scan.skills_count || 0) + (scan.commands_count || 0) > 0,
			detail: [
				scan.skills_count ? `${scan.skills_count} skill(s)` : null,
				scan.commands_count ? `${scan.commands_count} command(s)` : null,
			]
				.filter(Boolean)
				.join(", "),
		},
		{
			key: "memory",
			label: "Memory",
			available: scan.has_memory,
			detail: "CLAUDE.md",
		},
	];
	const anySelected = categories.some((c) => c.available && selection[c.key]);

	return (
		<div>
			<SectionHeading title="Claude Code" />
			<p className="text-xs text-[var(--muted)] leading-relaxed mb-3 max-w-[600px]">
				Import data from your Claude Code and Claude Desktop installation. This is a read-only copy {"\u2014"} your
				Claude files will not be modified.
			</p>
			{error ? (
				<div role="alert" className="alert-error-text whitespace-pre-line mb-3 max-w-[600px]">
					<span className="text-[var(--error)] font-medium">Error:</span> {error}
				</div>
			) : null}
			{done && result ? (
				<ImportResultPanel result={result} onReset={resetImport} />
			) : (
				<ImportCategoryGrid
					categories={categories.map((category) => ({
						...category,
						icon: CATEGORY_ICONS[category.key],
					}))}
					selection={selection}
					importing={importing}
					onToggle={toggleCategory}
				/>
			)}
			{done ? null : (
				<button
					type="button"
					className="provider-btn mt-3 w-fit"
					onClick={doImport}
					disabled={!anySelected || importing}
				>
					{importing ? "Importing\u2026" : "Import Selected"}
				</button>
			)}
		</div>
	);
}
