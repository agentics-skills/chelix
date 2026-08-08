import type { VNode } from "preact";

export interface ImportCategoryOption {
	key: string;
	label: string;
	available?: boolean;
	detail?: string;
	icon?: string;
}

export interface ImportCategoryResult {
	category: string;
	status: string;
	items_imported: number;
	items_skipped: number;
}

export interface ImportResultData {
	categories?: ImportCategoryResult[];
	total_imported?: number;
}

const IMPORT_STATUS_MARKERS: Record<string, string> = {
	success: "\u2713",
	partial: "~",
	skipped: "-",
};

function importCategoryClass(category: ImportCategoryOption, checked: boolean): string {
	const base = "flex items-center gap-3 p-3 rounded-md border text-left cursor-pointer transition-colors";
	if (!category.available) return `${base} border-[var(--border)] bg-[var(--surface)] opacity-40 cursor-not-allowed`;
	return checked
		? `${base} border-[var(--accent)] bg-[var(--accent-bg,rgba(var(--accent-rgb,59,130,246),0.08))]`
		: `${base} border-[var(--border)] bg-[var(--surface)] opacity-60`;
}

function ImportCategoryDetail({ category }: { category: ImportCategoryOption }): VNode | null {
	if (!category.available) return <div className="text-xs text-[var(--muted)] mt-0.5">not found</div>;
	return category.detail ? <div className="text-xs text-[var(--muted)] mt-0.5">{category.detail}</div> : null;
}

interface ImportCategoryCardProps {
	category: ImportCategoryOption;
	checked: boolean;
	importing: boolean;
	onToggle: (key: string) => void;
}

function ImportCategoryCard({ category, checked, importing, onToggle }: ImportCategoryCardProps): VNode {
	return (
		<button
			type="button"
			onClick={() => {
				if (category.available && !importing) onToggle(category.key);
			}}
			disabled={!category.available || importing}
			className={importCategoryClass(category, checked)}
		>
			<span className="text-lg shrink-0">{category.icon || "\uD83D\uDCE6"}</span>
			<div className="flex-1 min-w-0">
				<span className="text-sm font-medium text-[var(--text-strong)]">{category.label}</span>
				<ImportCategoryDetail category={category} />
			</div>
			<div className="shrink-0">
				{checked ? (
					<span className="icon icon-check-circle text-[var(--accent)]" />
				) : (
					<span className="w-4 h-4 rounded-full border-2 border-[var(--border)] inline-block" />
				)}
			</div>
		</button>
	);
}

interface ImportCategoryGridProps {
	categories: ImportCategoryOption[];
	selection: Record<string, boolean>;
	importing: boolean;
	onToggle: (key: string) => void;
}

export function ImportCategoryGrid({ categories, selection, importing, onToggle }: ImportCategoryGridProps): VNode {
	return (
		<div className="grid grid-cols-1 sm:grid-cols-2 gap-2 max-w-[600px]">
			{categories.map((category) => (
				<ImportCategoryCard
					key={category.key}
					category={category}
					checked={Boolean(selection[category.key] && category.available)}
					importing={importing}
					onToggle={onToggle}
				/>
			))}
		</div>
	);
}

function ImportResultRow({ category }: { category: ImportCategoryResult }): VNode {
	return (
		<div className="text-xs text-[var(--text)]">
			<span className="font-mono">[{IMPORT_STATUS_MARKERS[category.status] || "!"}]</span> {category.category}:{" "}
			{category.items_imported} imported, {category.items_skipped} skipped
		</div>
	);
}

interface ImportResultPanelProps {
	result: ImportResultData;
	onReset: () => void;
}

export function ImportResultPanel({ result, onReset }: ImportResultPanelProps): VNode {
	return (
		<div className="flex flex-col gap-2 max-w-[600px]">
			<div className="text-sm font-medium text-[var(--ok)]">
				Import complete: {result.total_imported || 0} item(s) imported.
			</div>
			{result.categories ? (
				<div className="flex flex-col gap-1">
					{result.categories.map((category) => (
						<ImportResultRow key={category.category} category={category} />
					))}
				</div>
			) : null}
			<button type="button" className="provider-btn provider-btn-secondary mt-2 w-fit" onClick={onReset}>
				Import Again
			</button>
		</div>
	);
}
