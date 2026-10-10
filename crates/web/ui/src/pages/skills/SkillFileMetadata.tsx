import type { VNode } from "preact";

export interface SkillFileDocument {
	name?: string;
	slug?: string | null;
	display_name?: string | null;
	description?: string;
	homepage?: string | null;
	license?: string | null;
	compatibility?: string | null;
	origin?: { source?: string | null; url?: string | null; version?: string | null } | null;
	allowed_agents?: string[];
	denied_agents?: string[];
	body?: string;
}

interface SkillFileFieldProps {
	label: string;
	value: string;
}

function Field({ label, value }: SkillFileFieldProps): VNode {
	return (
		<div className="flex gap-2">
			<span className="text-[var(--muted)]">{label}</span>
			<span className="font-mono text-[var(--text-strong)]">{value}</span>
		</div>
	);
}

interface SkillFileMetadataProps {
	file: SkillFileDocument;
}

export function SkillFileMetadata({ file }: SkillFileMetadataProps): VNode {
	const origin = [file.origin?.source, file.origin?.version, file.origin?.url].filter(Boolean).join(" ");
	return (
		<div className="mb-2 flex flex-col gap-1 border-b border-[var(--border)] px-2.5 py-2 text-xs text-[var(--text)]">
			{file.name ? <Field label="name" value={file.name} /> : null}
			{file.slug ? <Field label="slug" value={file.slug} /> : null}
			{file.display_name ? <Field label="display_name" value={file.display_name} /> : null}
			{file.description ? <Field label="description" value={file.description} /> : null}
			{file.homepage ? <Field label="homepage" value={file.homepage} /> : null}
			{file.license ? <Field label="license" value={file.license} /> : null}
			{file.compatibility ? <Field label="compatibility" value={file.compatibility} /> : null}
			{origin ? <Field label="origin" value={origin} /> : null}
			<Field label="allowed_agents" value={(file.allowed_agents ?? []).join(", ") || "—"} />
			<Field label="denied_agents" value={(file.denied_agents ?? []).join(", ") || "—"} />
		</div>
	);
}
