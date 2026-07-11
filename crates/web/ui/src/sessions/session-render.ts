// ── Session rendering: history messages, welcome card, session list ──

import {
	appendChannelFooter,
	appendReasoningDisclosure,
	chatAddMsg,
	chatAddMsgWithImages,
	highlightAndScroll,
	removeThinking,
	scrollChatToBottom,
	stripChannelPrefix,
	syncChatFollowStateFromPosition,
	updateCommandInputUI,
	updateTokenBar,
} from "../chat-ui";
import { highlightCodeBlocks } from "../code-highlight";
import * as gon from "../gon";
import { parseAgentsListPayload, renderAudioPlayer, renderDocument, renderMarkdown, sendRpc } from "../helpers";
import { appendMessageActions, appendUserMessageActions } from "../message-actions";
import { upsertTtsProviderFooter } from "../message-voice";
import { navigate } from "../router";
import { settingsPath } from "../routes";
import * as S from "../state";
import { modelStore } from "../stores/model-store";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import { terminalContextTokens } from "../terminal-usage";
import {
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	isCommandToolName,
	renderToolCardError,
	renderToolCardResult,
	toolCallIds,
} from "../tool-call-card";
import type { HistoryMessage } from "../types";
import type { ToolResult } from "../types/ws-events";

import { computeHistoryTailIndex, ensureHistoryScrollBinding, syncHistoryState } from "./session-history";
import { markSessionTailLocallyTruncated } from "./session-tail";

// ── Types ────────────────────────────────────────────────────

export interface SearchContext {
	query: string;
	messageIndex: number;
}

interface ToolResultMsg extends HistoryMessage {
	tool_call_id?: string;
	tool_name?: string;
	arguments?: unknown;
	success?: boolean;
	result?: ToolResult;
	error?: string;
	reasoning?: string;
}

interface AssistantMsg extends HistoryMessage {
	content?: string;
	model?: string;
	reasoningEffort?: string;
	provider?: string;
	inputTokens?: number;
	outputTokens?: number;
	cacheReadTokens?: number;
	cacheWriteTokens?: number;
	durationMs?: number;
	reasoning?: string;
	audio?: string;
	tts_provider?: string;
	run_id?: string;
	historyIndex?: number;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
	tool_calls?: unknown[];
	created_at?: number;
}

interface PendingTerminalToolMetadata {
	message: AssistantMsg;
	remaining: Set<string>;
	lastToolCard: HTMLElement | null;
}

interface UserMsg extends Omit<HistoryMessage, "content"> {
	content?: string | unknown[];
	historyIndex?: number;
	documents?: Array<{
		display_name?: string;
		stored_filename?: string;
		mime_type?: string;
		size_bytes?: number;
		media_ref?: string;
	}>;
	channel?: {
		channel_type?: string;
		username?: string;
		sender_name?: string;
		message_kind?: string;
	};
	audio?: string;
}

type TruncateTailEntry = Parameters<typeof markSessionTailLocallyTruncated>[2];

interface TruncateTailPayload {
	sessionKey?: string;
	keptCount?: number;
	entry?: TruncateTailEntry;
}

interface AgentInfo {
	id?: string;
	name?: string;
	emoji?: string;
}

/** History message with an optional seq field, used for resuming chat sequence counters. */
interface SeqHistoryMessage extends HistoryMessage {
	seq?: number;
	created_at?: number;
}

/** Token usage counters returned by chat.context RPC. */
interface TokenUsage {
	contextWindow?: number;
	inputTokens?: number;
	outputTokens?: number;
	estimatedNextInputTokens?: number;
	currentInputTokens?: number;
	currentTotal?: number;
}

/** Execution environment info returned by chat.context RPC. */
interface ExecutionInfo {
	mode?: string;
	isRoot?: boolean;
	hostIsRoot?: boolean;
}

/** Payload returned by the chat.context RPC. */
interface ChatContextPayload {
	tokenUsage?: TokenUsage;
	supportsTools?: boolean;
	execution?: ExecutionInfo;
}

// ── Multimodal parsing ───────────────────────────────────────

/** Extract text and images from a multimodal content array. */
function parseMultimodalContent(blocks: unknown[]): { text: string; images: { dataUrl: string; name: string }[] } {
	let text = "";
	const images: { dataUrl: string; name: string }[] = [];
	for (const block of blocks as Array<{ type?: string; text?: string; image_url?: { url?: string } }>) {
		if (block.type === "text") {
			text = block.text || "";
		} else if (block.type === "image_url" && block.image_url?.url) {
			images.push({ dataUrl: block.image_url.url, name: "image" });
		}
	}
	return { text, images };
}

// ── History message renderers ────────────────────────────────

function renderHistoryUserMessage(msg: UserMsg): HTMLElement | null {
	let text = "";
	let images: { dataUrl: string; name: string }[] = [];
	if (Array.isArray(msg.content)) {
		const parsed = parseMultimodalContent(msg.content);
		text = msg.channel ? stripChannelPrefix(parsed.text) : parsed.text;
		images = parsed.images;
	} else {
		text = msg.channel ? stripChannelPrefix((msg.content as string) || "") : (msg.content as string) || "";
	}

	let el: HTMLElement | null;
	if (msg.audio) {
		el = chatAddMsg("user", "", true);
		if (el) {
			const filename = msg.audio.split("/").pop() || "";
			const audioSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(filename)}`;
			renderAudioPlayer(el, audioSrc);
			if (text) {
				const textWrap = document.createElement("div");
				textWrap.className = "mt-2";
				// Safe: renderMarkdown escapes user input before formatting tags.
				textWrap.insertAdjacentHTML("beforeend", renderMarkdown(text));
				el.appendChild(textWrap);
			}
			if (images.length > 0) {
				const thumbRow = document.createElement("div");
				thumbRow.className = "msg-image-row";
				for (const img of images) {
					const thumb = document.createElement("img");
					thumb.className = "msg-image-thumb";
					thumb.src = img.dataUrl;
					thumb.alt = img.name;
					thumbRow.appendChild(thumb);
				}
				el.appendChild(thumbRow);
			}
		}
	} else if (images.length > 0) {
		el = chatAddMsgWithImages("user", text ? renderMarkdown(text) : "", images);
	} else {
		el = chatAddMsg("user", renderMarkdown(text), true);
	}
	if (el && Array.isArray(msg.documents)) {
		for (const doc of msg.documents) {
			const storedName = doc.stored_filename || doc.media_ref?.split("/").pop() || "";
			if (!storedName) continue;
			const mediaSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(storedName)}`;
			renderDocument(el, mediaSrc, doc.display_name || storedName, doc.mime_type, doc.size_bytes);
		}
	}
	appendUserMessageActions({
		messageEl: el,
		sessionKey: S.activeSessionKey,
		messageIndex: msg.historyIndex,
		text,
		onDeleted: (payload) => handleUserMessageDeleted(el, payload),
	});
	if (el && msg.channel) appendChannelFooter(el, msg.channel);
	return el;
}

function handleUserMessageDeleted(messageEl: HTMLElement | null, payload: unknown): void {
	const data = payload as TruncateTailPayload | null;
	const sessionKey = data?.sessionKey || S.activeSessionKey;
	markSessionTailLocallyTruncated(sessionKey, Number(data?.keptCount) || 0, data?.entry);
	if (sessionKey !== S.activeSessionKey || !location.pathname.startsWith("/chats/")) return;
	removeMessageTailFromDom(messageEl);
}

function removeMessageTailFromDom(messageEl: HTMLElement | null): void {
	let current = messageEl;
	while (current) {
		const next = current.nextElementSibling as HTMLElement | null;
		current.remove();
		current = next;
	}
}

function isTerminalAssistantMessage(msg: AssistantMsg): boolean {
	return msg.durationMs !== undefined || !Array.isArray(msg.tool_calls) || msg.tool_calls.length === 0;
}

function hasVisibleAssistantContent(msg: AssistantMsg): boolean {
	return Boolean(msg.content?.trim() || msg.reasoning?.trim() || msg.audio);
}

function applyTerminalAssistantUsage(msg: AssistantMsg, isTerminal: boolean): void {
	if (!isTerminal) return;
	if (msg.inputTokens || msg.outputTokens) {
		S.sessionTokens.input += msg.inputTokens || 0;
		S.sessionTokens.output += msg.outputTokens || 0;
	}
	if (msg.requestInputTokens !== undefined && msg.requestInputTokens !== null) {
		S.setSessionCurrentInputTokens(msg.requestInputTokens || 0);
	} else if (msg.inputTokens || msg.outputTokens) {
		S.setSessionCurrentInputTokens(msg.inputTokens || 0);
	}
	S.setSessionCurrentContextTokens(terminalContextTokens(msg));
}

function renderHistoryAssistantMessage(msg: AssistantMsg): HTMLElement | null {
	const isTerminal = isTerminalAssistantMessage(msg);
	if (!hasVisibleAssistantContent(msg)) {
		applyTerminalAssistantUsage(msg, isTerminal);
		return null;
	}
	let el: HTMLElement | null;
	if (msg.audio) {
		el = chatAddMsg("assistant", "", true);
		if (el) {
			const filename = msg.audio.split("/").pop() || "";
			const audioSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(filename)}`;
			renderAudioPlayer(el, audioSrc);
			if (msg.content) {
				const textWrap = document.createElement("div");
				textWrap.className = "mt-2";
				textWrap.insertAdjacentHTML("beforeend", renderMarkdown(msg.content));
				el.appendChild(textWrap);
			}
			if (msg.reasoning) {
				appendReasoningDisclosure(el, msg.reasoning);
			}
		}
	} else {
		el = chatAddMsg("assistant", renderMarkdown(msg.content || ""), true);
		if (el && msg.reasoning) {
			appendReasoningDisclosure(el, msg.reasoning);
		}
	}
	if (el && msg.model && isTerminal) {
		upsertTtsProviderFooter(el, msg.tts_provider);
		appendMessageActions({
			messageEl: el,
			sessionKey: S.activeSessionKey,
			messageIndex: msg.historyIndex,
			text: msg.content || "",
			runId: msg.run_id || undefined,
			hasAudio: !!msg.audio,
		});
	}
	if (el && Number.isInteger(msg.historyIndex)) {
		el.dataset.historyIndex = String(msg.historyIndex);
	}
	applyTerminalAssistantUsage(msg, isTerminal);
	return el;
}

function renderHistoryToolResult(msg: ToolResultMsg): HTMLElement {
	const success = msg.success !== false;
	const card = createToolCallCard({
		toolCallId: msg.tool_call_id,
		toolName: msg.tool_name,
		arguments: msg.arguments,
		status: success ? "success" : "error",
		expanded: isCommandToolName(msg.tool_name),
	});

	if (msg.result) {
		renderToolCardResult(card, msg.result, {
			sessionKey: S.activeSessionKey || "main",
			screenshotMode: "media",
		});
	}

	if (!msg.success && msg.error) {
		if (msg.result) {
			appendToolCardError(card, msg.error);
		} else {
			renderToolCardError(card, msg.error);
		}
	}

	if (msg.reasoning) {
		appendReasoningDisclosure(getToolCardDetailsContainer(card), msg.reasoning);
	}

	if (S.chatMsgBox) S.chatMsgBox.appendChild(card);
	return card;
}

function makeThinkingDots(): HTMLElement {
	const tpl = S.$<HTMLTemplateElement>("tpl-thinking-dots")!;
	return (tpl.content.cloneNode(true) as DocumentFragment).firstElementChild as HTMLElement;
}

export function postHistoryLoadActions(
	key: string,
	searchContext: SearchContext | null,
	msgEls: (HTMLElement | null)[],
	thinkingText: string | null,
	skipAutoScroll: boolean,
): void {
	sendRpc("chat.context", {}).then((ctxRes) => {
		if (ctxRes?.ok && ctxRes.payload) {
			const p = ctxRes.payload as ChatContextPayload;
			if (p.tokenUsage) {
				const tu = p.tokenUsage;
				S.setSessionContextWindow(tu.contextWindow || 0);
				S.setSessionTokens({
					input: tu.inputTokens || 0,
					output: tu.outputTokens || 0,
				});
				S.setSessionCurrentInputTokens(tu.estimatedNextInputTokens || tu.currentInputTokens || tu.inputTokens || 0);
				S.setSessionCurrentContextTokens(tu.currentTotal || tu.estimatedNextInputTokens || tu.currentInputTokens || 0);
			}
			S.setSessionToolsEnabled(p.supportsTools !== false);
			const execution = p.execution || {};
			const mode = execution.mode === "sandbox" ? "sandbox" : "host";
			const hostIsRoot = execution.hostIsRoot === true;
			let isRoot = execution.isRoot;
			if (typeof isRoot !== "boolean") {
				isRoot = mode === "sandbox" ? true : hostIsRoot;
			}
			S.setHostCommandIsRoot(hostIsRoot);
			S.setSessionCommandMode(mode);
			S.setSessionCommandPromptSymbol(isRoot ? "#" : "$");
		}
		updateCommandInputUI();
		updateTokenBar();
	});
	updateTokenBar();

	if (!skipAutoScroll && searchContext?.query && S.chatMsgBox) {
		highlightAndScroll(msgEls, searchContext.messageIndex, searchContext.query);
	} else if (skipAutoScroll) {
		syncChatFollowStateFromPosition();
	} else {
		scrollChatToBottom(true);
	}

	const session = sessionStore.getByKey(key);
	if (session?.replying.value && S.chatMsgBox) {
		removeThinking();
		const thinkEl = document.createElement("div");
		thinkEl.className = "msg assistant thinking";
		thinkEl.id = "thinkingIndicator";
		if (thinkingText) {
			const textEl = document.createElement("span");
			textEl.className = "thinking-text";
			textEl.textContent = thinkingText;
			thinkEl.appendChild(textEl);
		} else {
			thinkEl.appendChild(makeThinkingDots());
		}
		S.chatMsgBox.appendChild(thinkEl);
		if (!skipAutoScroll) scrollChatToBottom(true);
	}
}

/** No-op -- the Preact SessionHeader component auto-updates from signals. */
export function updateChatSessionHeader(): void {
	// Retained for backward compat call sites; Preact handles rendering.
}

export function renderWelcomeAgentPicker(
	card: HTMLElement,
	activeAgentId: string,
	onActiveAgentResolved: (agent: AgentInfo | null) => void,
): void {
	const container = card.querySelector("[data-welcome-agents]") as HTMLElement | null;
	if (!container) return;

	sendRpc("agents.list", {}).then((res) => {
		if (!card.isConnected) return;
		if (!res?.ok) {
			container.classList.add("hidden");
			return;
		}
		const parsed = parseAgentsListPayload(res.payload as Parameters<typeof parseAgentsListPayload>[0]);
		const agents = (parsed.agents || []) as AgentInfo[];
		const defaultId = (parsed.defaultId || "main") as string;
		const effectiveActive = activeAgentId || defaultId;

		container.textContent = "";
		container.classList.remove("hidden");
		container.classList.add("flex");

		let activeAgent: AgentInfo | null = null;
		for (const agent of agents) {
			if (!agent?.id) continue;
			if (agent.id === effectiveActive) activeAgent = agent;
			const chip = document.createElement("button");
			chip.type = "button";
			chip.className = agent.id === effectiveActive ? "provider-btn" : "provider-btn provider-btn-secondary";
			chip.style.fontSize = "0.7rem";
			chip.style.padding = "3px 8px";
			const labelPrefix = agent.emoji ? `${agent.emoji} ` : "";
			chip.textContent = `${labelPrefix}${agent.name || agent.id}`;
			chip.addEventListener("click", () => {
				const key = sessionStore.activeSessionKey.value || S.activeSessionKey || "main";
				sendRpc("agents.set_session", { session_key: key, agent_id: agent.id }).then((setRes) => {
					if (!setRes?.ok) return;
					const live = sessionStore.getByKey(key);
					if (live) {
						live.agent_id = agent.id || "";
						live.dataVersion.value++;
					}
					// Lazy import to avoid circular dependency with sessions.ts
					void import("../sessions").then(({ fetchSessions }) => fetchSessions());
					const welcome = S.chatMsgBox?.querySelector("#welcomeCard");
					if (welcome) {
						welcome.remove();
						showWelcomeCard();
					}
				});
			});
			container.appendChild(chip);
		}

		const hatchBtn = document.createElement("button");
		hatchBtn.type = "button";
		hatchBtn.className = "provider-btn provider-btn-secondary";
		hatchBtn.style.fontSize = "0.7rem";
		hatchBtn.style.padding = "3px 8px";
		hatchBtn.textContent = "\u{1F95A} Hatch a new agent";
		hatchBtn.addEventListener("click", () => {
			navigate(settingsPath("agents/new"));
		});
		container.appendChild(hatchBtn);

		onActiveAgentResolved(activeAgent);
	});
}

function showWelcomeCard(): void {
	if (!S.chatMsgBox) return;
	S.chatMsgBox.classList.add("chat-messages-empty");

	if (modelStore.models.value.length === 0) {
		const noProvTpl = S.$<HTMLTemplateElement>("tpl-no-providers-card");
		if (!noProvTpl) return;
		const noProvCard = (noProvTpl.content.cloneNode(true) as DocumentFragment).firstElementChild as HTMLElement;
		S.chatMsgBox.appendChild(noProvCard);
		return;
	}

	const tpl = S.$<HTMLTemplateElement>("tpl-welcome-card");
	if (!tpl) return;
	const card = (tpl.content.cloneNode(true) as DocumentFragment).firstElementChild as HTMLElement;
	const identity = gon.get("identity");
	const userName = identity?.user_name;
	const botName = identity?.name || "chelix";
	const botEmoji = identity?.emoji || "";

	const greetingEl = card.querySelector("[data-welcome-greeting]") as HTMLElement | null;
	if (greetingEl) greetingEl.textContent = userName ? `Hello, ${userName}!` : "Hello!";
	const emojiEl = card.querySelector("[data-welcome-emoji]") as HTMLElement | null;
	if (emojiEl) emojiEl.textContent = botEmoji;
	const nameEl = card.querySelector("[data-welcome-bot-name]") as HTMLElement | null;
	if (nameEl) nameEl.textContent = botName;
	const activeAgentId = sessionStore.activeSession.value?.agent_id || "main";
	renderWelcomeAgentPicker(card, activeAgentId, (activeAgent) => {
		if (!activeAgent) return;
		if (emojiEl) emojiEl.textContent = activeAgent.emoji || "";
		if (nameEl) nameEl.textContent = activeAgent.name || botName;
	});

	S.chatMsgBox.appendChild(card);
}

export function refreshWelcomeCardIfNeeded(): void {
	if (!S.chatMsgBox) return;
	const welcomeCard = S.chatMsgBox.querySelector("#welcomeCard");
	const noProvCard = S.chatMsgBox.querySelector("#noProvidersCard");
	const hasModels = modelStore.models.value.length > 0;

	if (hasModels && noProvCard) {
		noProvCard.remove();
		showWelcomeCard();
	} else if (!hasModels && welcomeCard) {
		welcomeCard.remove();
		showWelcomeCard();
	}
}

export function showSessionLoadIndicator(): void {
	if (!S.chatMsgBox) return;
	hideSessionLoadIndicator();
	const loading = document.createElement("div");
	loading.id = "sessionLoadIndicator";
	loading.className = "msg assistant thinking session-loading";
	loading.appendChild(makeThinkingDots());
	const label = document.createElement("span");
	label.className = "session-loading-label";
	label.textContent = "Loading session\u2026";
	loading.appendChild(label);
	S.chatMsgBox.appendChild(loading);
}

export function hideSessionLoadIndicator(): void {
	const loading = document.getElementById("sessionLoadIndicator");
	if (loading) loading.remove();
}

export function renderHistory(
	key: string,
	history: HistoryMessage[],
	searchContext: SearchContext | null,
	thinkingText: string | null,
	totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	ensureHistoryScrollBinding();
	hideSessionLoadIndicator();
	if (S.chatMsgBox) {
		S.chatMsgBox.classList.remove("chat-messages-empty");
		S.chatMsgBox.textContent = "";
	}
	const msgEls: (HTMLElement | null)[] = [];
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	S.setChatBatchLoading(true);
	const pendingTerminalToolMetadata = new Map<string, PendingTerminalToolMetadata>();
	history.forEach((msg) => {
		if (msg.role === "user") {
			msgEls.push(renderHistoryUserMessage(msg as UserMsg));
		} else if (msg.role === "assistant") {
			const assistantMessage = msg as AssistantMsg;
			const assistantEl = renderHistoryAssistantMessage(assistantMessage);
			msgEls.push(assistantEl);
			if (!isTerminalAssistantMessage(assistantMessage)) {
				return;
			}
			const toolIds = toolCallIds(assistantMessage.tool_calls);
			if (toolIds.length === 0) {
				appendTerminalMetadata(
					S.chatMsgBox,
					assistantEl,
					terminalMetadataData(assistantMessage, { historyIndex: assistantMessage.historyIndex }),
				);
			} else {
				const pending: PendingTerminalToolMetadata = {
					message: assistantMessage,
					remaining: new Set(toolIds),
					lastToolCard: null,
				};
				for (const toolCallId of toolIds) pendingTerminalToolMetadata.set(toolCallId, pending);
			}
		} else if (msg.role === "notice") {
			msgEls.push(chatAddMsg("system", renderMarkdown(typeof msg.content === "string" ? msg.content : ""), true));
		} else if (msg.role === "tool_result") {
			const toolResult = msg as ToolResultMsg;
			const toolCard = renderHistoryToolResult(toolResult);
			msgEls.push(toolCard);
			const pending = toolResult.tool_call_id ? pendingTerminalToolMetadata.get(toolResult.tool_call_id) : undefined;
			if (pending && toolResult.tool_call_id) {
				pending.remaining.delete(toolResult.tool_call_id);
				pending.lastToolCard = toolCard;
				if (pending.remaining.size === 0) {
					for (const toolCallId of toolCallIds(pending.message.tool_calls)) {
						pendingTerminalToolMetadata.delete(toolCallId);
					}
					appendTerminalMetadata(
						S.chatMsgBox,
						toolCard,
						terminalMetadataData(pending.message, { historyIndex: pending.message.historyIndex }),
					);
				}
			}
		} else {
			msgEls.push(null);
		}
	});
	for (const pending of new Set(pendingTerminalToolMetadata.values())) {
		if (!pending.lastToolCard) continue;
		appendTerminalMetadata(
			S.chatMsgBox,
			pending.lastToolCard,
			terminalMetadataData(pending.message, { historyIndex: pending.message.historyIndex }),
		);
	}
	S.setChatBatchLoading(false);
	if (S.chatMsgBox) highlightCodeBlocks(S.chatMsgBox);
	const historyTailIndex = computeHistoryTailIndex(history);
	syncHistoryState(key, history, historyTailIndex, totalCountHint);

	let maxSeq = 0;
	for (const hm of history) {
		if (hm.role === "user" && ((hm as SeqHistoryMessage).seq as number) > maxSeq) {
			maxSeq = (hm as SeqHistoryMessage).seq as number;
		}
	}
	S.setChatSeq(maxSeq);
	if (history.length === 0) {
		showWelcomeCard();
	}
	postHistoryLoadActions(key, searchContext, msgEls, thinkingText, skipAutoScroll === true);
}
