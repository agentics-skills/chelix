// ── API key help text for providers ───────────────────────

interface KeySource {
	url: string;
	label: string;
}

interface ProviderInfo {
	name: string;
	displayName: string;
	keyOptional?: boolean;
}

export interface ApiKeyHelp {
	text: string;
	url?: string;
	label?: string;
}

const KEY_SOURCE_BY_PROVIDER: Record<string, KeySource> = {
	openai: {
		url: "https://platform.openai.com/api-keys",
		label: "OpenAI Platform",
	},
	openrouter: {
		url: "https://openrouter.ai/settings/keys",
		label: "OpenRouter Settings",
	},
};

export function providerApiKeyHelp(provider: ProviderInfo | null): ApiKeyHelp | null {
	if (!provider) return null;

	if (provider.keyOptional) {
		return {
			text: `API key is optional for ${provider.displayName}. Leave blank unless your gateway requires one.`,
		};
	}

	const source = KEY_SOURCE_BY_PROVIDER[provider.name];
	if (source) {
		return {
			text: "Get your key at",
			url: source.url,
			label: source.label,
		};
	}

	return {
		text: `Get your API key from the ${provider.displayName} dashboard.`,
	};
}
