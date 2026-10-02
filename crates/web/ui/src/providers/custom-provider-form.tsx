import type { VNode } from "preact";
import { useEffect, useRef, useState } from "preact/hooks";
import { sendRpc } from "../helpers";
import { providerBaseUrlError } from "../provider-validation";
import { targetValue } from "../typed-events";
import type { ProviderInfo } from "../types/model";

type WireApi = "chat-completions" | "responses";
type ToolMode = "native" | "text" | "off";

function slugFromName(name: string): string {
	return name.startsWith("custom-") ? name.slice("custom-".length) : name;
}

function normalizedSlug(raw: string): string {
	let slug = raw.trim().toLowerCase();
	if (slug.startsWith("custom-")) slug = slug.slice("custom-".length);
	return slug;
}

function storedNamePreview(provider: ProviderInfo | null, raw: string): string {
	if (provider && (raw === slugFromName(provider.name) || raw === provider.name)) return provider.name;
	const slug = normalizedSlug(raw);
	return slug ? `custom-${slug}` : "custom-name";
}

export function CustomProviderForm(props: {
	provider: ProviderInfo | null;
	onCancel: () => void;
	onSaved: (providerName: string) => void;
	onSavingChange?: (saving: boolean) => void;
}): VNode {
	const editing = props.provider != null;
	const alive = useRef(true);
	useEffect(() => {
		return () => {
			alive.current = false;
		};
	}, []);
	const [name, setName] = useState(props.provider ? slugFromName(props.provider.name) : "");
	const [baseUrl, setBaseUrl] = useState(props.provider?.baseUrl || "");
	const [apiKey, setApiKey] = useState("");
	const [wireApi, setWireApi] = useState<WireApi>(props.provider?.wireApi || "chat-completions");
	const [toolMode, setToolMode] = useState<ToolMode>(props.provider?.toolMode || "native");
	const [enabled, setEnabled] = useState(props.provider?.enabled !== false);
	const [error, setError] = useState<string | null>(null);
	const [saving, setSaving] = useState(false);

	function onSubmit(event: Event): void {
		event.preventDefault();
		if (saving) return;
		const trimmedName = name.trim();
		const trimmedUrl = baseUrl.trim();
		const trimmedKey = apiKey.trim();
		if (!trimmedName) {
			setError("Name is required.");
			return;
		}
		if (!trimmedUrl) {
			setError("Endpoint URL is required.");
			return;
		}
		if (!(editing || trimmedKey)) {
			setError("API key is required.");
			return;
		}
		const endpointError = providerBaseUrlError(trimmedUrl, wireApi === "responses");
		if (endpointError) {
			setError(endpointError);
			return;
		}
		setSaving(true);
		props.onSavingChange?.(true);
		setError(null);
		void sendRpc<{ ok: boolean; providerName: string }>("providers.upsert_custom", {
			name: trimmedName,
			previousName: props.provider?.name,
			baseUrl: trimmedUrl,
			apiKey: trimmedKey,
			wireApi,
			toolMode,
			enabled,
			streamTransport: "sse",
		})
			.then((response) => {
				if (!alive.current) return;
				setSaving(false);
				props.onSavingChange?.(false);
				if (!(response?.ok && response.payload?.providerName)) {
					setError(response?.error?.message || "Failed to save provider.");
					return;
				}
				props.onSaved(response.payload.providerName);
			})
			.catch((err: Error) => {
				if (!alive.current) return;
				setSaving(false);
				props.onSavingChange?.(false);
				setError(err?.message || "Failed to save provider.");
			});
	}

	return (
		<form onSubmit={onSubmit} className="provider-key-form flex flex-col gap-2">
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">Name</span>
				<input
					id="customProviderName"
					className="provider-key-input w-full"
					value={name}
					onInput={(event) => setName(targetValue(event))}
					placeholder="my-endpoint"
					disabled={saving}
				/>
				<span className="text-xs text-[var(--muted)]">Stored as {storedNamePreview(props.provider, name)}.</span>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">Endpoint URL</span>
				<input
					id="customProviderEndpoint"
					className="provider-key-input w-full"
					value={baseUrl}
					onInput={(event) => setBaseUrl(targetValue(event))}
					placeholder="https://api.example.com/v1"
					disabled={saving}
				/>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">API Key</span>
				<input
					id="customProviderApiKey"
					type="password"
					className="provider-key-input w-full"
					value={apiKey}
					onInput={(event) => setApiKey(targetValue(event))}
					placeholder={editing ? "Leave empty to keep the current key" : "sk-..."}
					disabled={saving}
				/>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">wire_api</span>
				<select
					id="customProviderWireApi"
					className="provider-key-input w-full"
					value={wireApi}
					disabled={saving}
					onChange={(event) => setWireApi(targetValue(event) as WireApi)}
				>
					<option value="chat-completions">chat-completions</option>
					<option value="responses">responses</option>
				</select>
			</label>
			<label>
				<span className="text-xs text-[var(--muted)] mb-1 block">tool_mode</span>
				<select
					id="customProviderToolMode"
					className="provider-key-input w-full"
					value={toolMode}
					disabled={saving}
					onChange={(event) => setToolMode(targetValue(event) as ToolMode)}
				>
					<option value="native">native</option>
					<option value="text">text</option>
					<option value="off">off</option>
				</select>
			</label>
			<label className="flex items-center gap-2 text-xs text-[var(--text)]">
				<input
					type="checkbox"
					checked={enabled}
					disabled={saving}
					onChange={(event) => setEnabled(event.currentTarget.checked)}
				/>
				enabled
			</label>
			{error ? <div className="alert-error-text text-[var(--error)] whitespace-pre-line">{error}</div> : null}
			<div className="flex items-center gap-2">
				<button type="submit" className="provider-btn provider-btn-sm" disabled={saving}>
					{saving ? "Saving…" : editing ? "Save" : "Add Provider"}
				</button>
				<button
					type="button"
					className="provider-btn provider-btn-secondary provider-btn-sm"
					onClick={props.onCancel}
					disabled={saving}
				>
					Cancel
				</button>
			</div>
		</form>
	);
}
