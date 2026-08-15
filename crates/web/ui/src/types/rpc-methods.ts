// ── Typed RPC method registry ────────────────────────────────
//
// Maps every RPC method name to its response payload type so that
// `sendRpc("method.name", params)` infers the correct return type
// at compile time. Methods whose payload shapes are not yet fully
// typed use `unknown` as a placeholder -- callers can narrow with
// `as` casts until we refine the type here.

import type {
	ChatContextPayload,
	ChatFullContextPayload,
	ChatPromptMemoryRefreshPayload,
	ChatPromptQueuePayload,
} from "./chat";
import type { ModelInfo, ProviderInfo } from "./model";
import type { SessionMeta, SetSessionAgentPayload } from "./session";

/** Maps every RPC method to its response payload type. */
export interface RpcMethodMap {
	// ── A2UI ────────────────────────────────────────────────────
	"a2ui.action": { accepted: boolean };

	// ── Agents ──────────────────────────────────────────────────
	"agents.create": unknown;
	"agents.delete": unknown;
	"agents.get": unknown;
	"agents.list": unknown;
	"agents.set_default": unknown;
	"agents.set_session": SetSessionAgentPayload;
	"agents.update": unknown;

	// ── Channels ────────────────────────────────────────────────
	"channels.add": unknown;
	"channels.remove": unknown;
	"channels.retry_ownership": unknown;
	"channels.status": unknown;
	"channels.update": unknown;

	// ── Chat ────────────────────────────────────────────────────
	"chat.abort": unknown;
	"chat.clear": unknown;
	"chat.compact": unknown;
	"chat.context": ChatContextPayload;
	"chat.full_context": ChatFullContextPayload;
	"chat.prompt_memory.refresh": ChatPromptMemoryRefreshPayload;
	"chat.prompt_queue.cancel": ChatPromptQueuePayload;
	"chat.prompt_queue.list": ChatPromptQueuePayload;
	"chat.send": unknown;
	"chat.send_sync": unknown;

	// ── Cron ────────────────────────────────────────────────────
	"cron.list": unknown;
	"cron.remove": unknown;
	"cron.run": unknown;
	"cron.runs": unknown;
	"cron.status": unknown;
	"cron.update": unknown;

	// ── Command Approvals ───────────────────────────────────────
	"command.approval.resolve": unknown;
	"external_agents.bind": unknown;
	"external_agents.list": unknown;
	"external_agents.status": unknown;
	"external_agents.unbind": unknown;

	// ── GraphQL ─────────────────────────────────────────────────
	"graphql.config.get": unknown;
	"graphql.config.set": unknown;

	// ── Heartbeat ───────────────────────────────────────────────
	"heartbeat.run": unknown;
	"heartbeat.runs": unknown;
	"heartbeat.status": unknown;
	"heartbeat.update": unknown;

	// ── Hooks ───────────────────────────────────────────────────
	"hooks.list": unknown;
	"hooks.reload": unknown;
	"hooks.save": unknown;

	// ── Location ────────────────────────────────────────────────
	"location.result": unknown;

	// ── Logs ────────────────────────────────────────────────────
	"logs.ack": unknown;
	"logs.list": unknown;
	"logs.status": unknown;

	// ── MCP ─────────────────────────────────────────────────────
	"mcp.add": unknown;
	"mcp.config.get": unknown;
	"mcp.config.update": unknown;
	"mcp.list": unknown;
	"mcp.remove": unknown;
	"mcp.restart": unknown;
	"mcp.tools": unknown;
	"mcp.update": unknown;

	// ── Memory ──────────────────────────────────────────────────
	"memory.config.get": unknown;
	"memory.config.update": unknown;
	"memory.qmd.status": unknown;
	"memory.status": unknown;

	// ── Models ──────────────────────────────────────────────────
	"models.cancel_detect": unknown;
	"models.detect_supported": unknown;
	"models.disable": unknown;
	"models.enable": unknown;
	"models.list": ModelInfo[];
	"models.list_all": ModelInfo[];
	"models.test": unknown;

	// ── Projects ────────────────────────────────────────────────
	"projects.complete_path": unknown;
	"projects.delete": unknown;
	"projects.detect": unknown;
	"projects.list": unknown;
	"projects.upsert": unknown;

	// ── Providers ───────────────────────────────────────────────
	"providers.add_custom": unknown;
	"providers.available": ProviderInfo[];
	"providers.remove_key": unknown;
	"providers.save_key": unknown;
	"providers.save_models": unknown;
	"providers.validate_key": unknown;

	// ── Sessions ────────────────────────────────────────────────
	"sessions.clear_all": unknown;
	"sessions.delete": unknown;
	"sessions.patch": { result?: Record<string, unknown> };
	"sessions.search": SessionMeta[];
	"sessions.switch": unknown;
	"sessions.voice.generate": { audio?: string; ttsProvider?: string };

	// ── Skills ──────────────────────────────────────────────────
	"skills.emergency_disable": unknown;
	"skills.install": unknown;
	"skills.repos.export": unknown;
	"skills.repos.import": unknown;
	"skills.repos.remove": unknown;
	"skills.repos.unquarantine": unknown;
	"skills.skill.detail": unknown;
	"skills.skill.disable": unknown;
	"skills.bundled.categories": unknown;
	"skills.bundled.toggle_category": unknown;
	"skills.recipe": {
		found: boolean;
		recipe?: {
			source: string;
			title: string;
			instructions: string;
			mcp_servers: unknown[];
			skills_to_enable: string[];
		};
	};

	// ── STT (Speech-to-Text) ────────────────────────────────────
	"stt.status": unknown;

	// ── Subscribe ───────────────────────────────────────────────
	subscribe: unknown;

	// ── TTS (Text-to-Speech) ────────────────────────────────────
	"tts.convert": unknown;
	"tts.generate_phrase": { phrase: string; source: "llm" | "static" };
	"tts.status": unknown;

	// ── User profile ────────────────────────────────────────────
	"user.get": unknown;
	"user.update": unknown;

	// ── Voice ───────────────────────────────────────────────────
	"voice.config.save_key": unknown;
	"voice.config.save_settings": unknown;
	"voice.config.voxtral_requirements": unknown;
	"voice.elevenlabs.catalog": unknown;
	"voice.provider.toggle": unknown;
	"voice.providers.all": unknown;

	// ── Webhooks ────────────────────────────────────────────────
	"webhooks.delete": unknown;
	"webhooks.deliveries": unknown;
	"webhooks.list": unknown;
	"webhooks.update": unknown;
}

/** All valid RPC method names. */
export type RpcMethod = keyof RpcMethodMap;
