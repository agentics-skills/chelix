// ── Session rendering: history messages, welcome card, session list ──

import { getA2uiToolArgumentsError, isA2uiTool, mountA2uiToolCard } from "../a2ui-renderer";
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
	updateTokenBar,
} from "../chat-ui";
import { highlightCodeBlocks } from "../code-highlight";
import * as gon from "../gon";
import { parseAgentsListPayload, renderAudioPlayer, renderDocument, renderMarkdown, sendRpc } from "../helpers";
import { appendMessageActions, appendUserMessageActions } from "../message-actions";
import { upsertTtsProviderFooter } from "../message-voice";
import { renderCheckpointCard } from "../pages/chat/context-card";
import { navigate } from "../router";
import { settingsPath } from "../routes";
import * as S from "../state";
import { modelStore } from "../stores/model-store";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import { terminalContextTokens } from "../terminal-usage";
import {
	appendToolCardContextBudget,
	appendToolCardError,
	createToolCallCard,
	getToolCardDetailsContainer,
	isCommandToolName,
	renderToolCardError,
	renderToolCardResult,
	toolCallIds,
} from "../tool-call-card";
import type { RpcResponse } from "../types/rpc";
import type { HistoryMessage } from "../types/session";
import type { CheckpointHistoryMessage, ContextBudgetMetadata, ToolResult } from "../types/ws-events";
import { showToast } from "../ui";

import { setSessionAgent } from "./session-agent";
import { computeHistoryTailIndex, syncHistoryState } from "./session-history";
import { fetchSessions } from "./session-list";
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
	result?: ToolResult | string;
	error?: string;
	rejected?: boolean;
	reasoning?: string;
	run_id?: string;
	contextBudget?: ContextBudgetMetadata;
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

function userMessageContent(msg: UserMsg): { text: string; images: { dataUrl: string; name: string }[] } {
	const parsed = Array.isArray(msg.content)
		? parseMultimodalContent(msg.content)
		: { text: (msg.content as string) || "", images: [] };
	return {
		text: msg.channel ? stripChannelPrefix(parsed.text) : parsed.text,
		images: parsed.images,
	};
}

function appendImageThumbnails(messageEl: HTMLElement, images: { dataUrl: string; name: string }[]): void {
	if (images.length === 0) return;
	const thumbRow = document.createElement("div");
	thumbRow.className = "msg-image-row";
	for (const image of images) {
		const thumb = document.createElement("img");
		thumb.className = "msg-image-thumb";
		thumb.src = image.dataUrl;
		thumb.alt = image.name;
		thumbRow.appendChild(thumb);
	}
	messageEl.appendChild(thumbRow);
}

function renderUserAudioMessage(
	audio: string,
	text: string,
	images: { dataUrl: string; name: string }[],
): HTMLElement | null {
	const messageEl = chatAddMsg("user", "", true);
	if (!messageEl) return null;
	const filename = audio.split("/").pop() || "";
	const audioSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(filename)}`;
	renderAudioPlayer(messageEl, audioSrc);
	if (text) {
		const textWrap = document.createElement("div");
		textWrap.className = "mt-2";
		// Safe: renderMarkdown escapes user input before formatting tags.
		textWrap.insertAdjacentHTML("beforeend", renderMarkdown(text));
		messageEl.appendChild(textWrap);
	}
	appendImageThumbnails(messageEl, images);
	return messageEl;
}

function renderUserMessageBody(
	msg: UserMsg,
	text: string,
	images: { dataUrl: string; name: string }[],
): HTMLElement | null {
	if (msg.audio) return renderUserAudioMessage(msg.audio, text, images);
	if (images.length > 0) return chatAddMsgWithImages("user", text ? renderMarkdown(text) : "", images);
	return chatAddMsg("user", renderMarkdown(text), true);
}

function appendUserDocuments(messageEl: HTMLElement | null, documents: UserMsg["documents"]): void {
	if (!(messageEl && Array.isArray(documents))) return;
	for (const documentInfo of documents) {
		const storedName = documentInfo.stored_filename || documentInfo.media_ref?.split("/").pop() || "";
		if (!storedName) continue;
		const mediaSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(storedName)}`;
		renderDocument(
			messageEl,
			mediaSrc,
			documentInfo.display_name || storedName,
			documentInfo.mime_type,
			documentInfo.size_bytes,
		);
	}
}

function renderHistoryUserMessage(msg: UserMsg): HTMLElement | null {
	const { text, images } = userMessageContent(msg);
	const messageEl = renderUserMessageBody(msg, text, images);
	appendUserDocuments(messageEl, msg.documents);
	appendUserMessageActions({
		messageEl,
		sessionKey: S.activeSessionKey,
		messageIndex: msg.historyIndex,
		text,
		onDeleted: (payload) => handleUserMessageDeleted(messageEl, payload),
	});
	if (messageEl && msg.channel) appendChannelFooter(messageEl, msg.channel);
	return messageEl;
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

function renderAssistantAudioMessage(msg: AssistantMsg): HTMLElement | null {
	const messageEl = chatAddMsg("assistant", "", true);
	if (!(messageEl && msg.audio)) return messageEl;
	const filename = msg.audio.split("/").pop() || "";
	const audioSrc = `/api/sessions/${encodeURIComponent(S.activeSessionKey)}/media/${encodeURIComponent(filename)}`;
	renderAudioPlayer(messageEl, audioSrc);
	if (msg.content) {
		const textWrap = document.createElement("div");
		textWrap.className = "mt-2";
		textWrap.insertAdjacentHTML("beforeend", renderMarkdown(msg.content));
		messageEl.appendChild(textWrap);
	}
	return messageEl;
}

function renderAssistantMessageBody(msg: AssistantMsg): HTMLElement | null {
	const messageEl = msg.audio
		? renderAssistantAudioMessage(msg)
		: chatAddMsg("assistant", renderMarkdown(msg.content || ""), true);
	if (messageEl && msg.reasoning) appendReasoningDisclosure(messageEl, msg.reasoning);
	return messageEl;
}

function decorateAssistantMessage(messageEl: HTMLElement | null, msg: AssistantMsg): void {
	if (!messageEl) return;
	upsertTtsProviderFooter(messageEl, msg.tts_provider);
	appendMessageActions({
		messageEl,
		sessionKey: S.activeSessionKey,
		messageIndex: msg.historyIndex,
		text: msg.content || "",
		hasAudio: Boolean(msg.audio),
	});
	if (Number.isInteger(msg.historyIndex)) messageEl.dataset.historyIndex = String(msg.historyIndex);
}

function renderHistoryAssistantMessage(msg: AssistantMsg): HTMLElement | null {
	const isTerminal = isTerminalAssistantMessage(msg);
	if (!hasVisibleAssistantContent(msg)) {
		applyTerminalAssistantUsage(msg, isTerminal);
		return null;
	}
	const messageEl = renderAssistantMessageBody(msg);
	decorateAssistantMessage(messageEl, msg);
	applyTerminalAssistantUsage(msg, isTerminal);
	return messageEl;
}

function appendToolResultSections(
	card: HTMLElement,
	msg: ToolResultMsg,
	rejected: boolean,
	a2uiArgumentsError: string | null,
): void {
	if (msg.result) {
		renderToolCardResult(card, msg.result, {
			sessionKey: S.activeSessionKey || "main",
			screenshotMode: "media",
		});
	}
	const toolError = rejected ? msg.error || a2uiArgumentsError : msg.success === false ? msg.error : undefined;
	if (toolError && msg.result) appendToolCardError(card, toolError, rejected);
	if (toolError && !msg.result) renderToolCardError(card, toolError, rejected);
	appendToolCardContextBudget(card, msg.contextBudget);
}

function appendA2uiToolSurface(card: HTMLElement, msg: ToolResultMsg, rejected: boolean, success: boolean): void {
	if (!isA2uiTool(msg.tool_name) || rejected) return;
	mountA2uiToolCard(card, {
		arguments: msg.arguments,
		runId: msg.run_id,
		toolCallId: msg.tool_call_id,
		interactive: false,
		success,
		result: msg.result,
		error: msg.error,
	});
}

function renderHistoryToolResult(msg: ToolResultMsg): HTMLElement {
	const success = msg.success !== false;
	const a2uiTool = isA2uiTool(msg.tool_name);
	const a2uiArgumentsError = a2uiTool ? getA2uiToolArgumentsError(msg.arguments) : null;
	const rejected = msg.rejected === true || a2uiArgumentsError !== null;
	const card = createToolCallCard({
		toolCallId: msg.tool_call_id,
		toolName: msg.tool_name,
		arguments: msg.arguments,
		status: rejected ? "retry" : success ? "success" : "error",
		expanded: rejected || a2uiTool || isCommandToolName(msg.tool_name),
	});
	appendToolResultSections(card, msg, rejected, a2uiArgumentsError);
	appendA2uiToolSurface(card, msg, rejected, success);
	if (msg.reasoning) appendReasoningDisclosure(getToolCardDetailsContainer(card), msg.reasoning);
	if (S.chatMsgBox) S.chatMsgBox.appendChild(card);
	return card;
}

function makeThinkingDots(): HTMLElement {
	const template = S.$<HTMLTemplateElement>("tpl-thinking-dots");
	if (!template) throw new Error("Thinking dots template is missing");
	const element = (template.content.cloneNode(true) as DocumentFragment).firstElementChild;
	if (!(element instanceof HTMLElement)) throw new Error("Thinking dots template is empty");
	return element;
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
			const p = ctxRes.payload;
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
		}
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

function refreshWelcomeAfterAgentChange(): void {
	fetchSessions();
	const welcome = S.chatMsgBox?.querySelector("#welcomeCard");
	if (!welcome) return;
	welcome.remove();
	showWelcomeCard();
}

function selectWelcomeAgent(chip: HTMLButtonElement, agentId: string): void {
	const key = sessionStore.activeSessionKey.value || S.activeSessionKey || "main";
	chip.disabled = true;
	void setSessionAgent(key, agentId)
		.then((response) => {
			if (!response.ok) {
				showToast(response.error?.message || "Failed to switch agent", "error");
				return;
			}
			refreshWelcomeAfterAgentChange();
		})
		.finally(() => {
			if (chip.isConnected) chip.disabled = false;
		});
}

function createWelcomeAgentChip(agent: AgentInfo, agentId: string, activeAgentId: string): HTMLButtonElement {
	const chip = document.createElement("button");
	chip.type = "button";
	chip.className = agentId === activeAgentId ? "provider-btn" : "provider-btn provider-btn-secondary";
	chip.style.fontSize = "0.7rem";
	chip.style.padding = "3px 8px";
	const labelPrefix = agent.emoji ? `${agent.emoji} ` : "";
	chip.textContent = `${labelPrefix}${agent.name || agentId}`;
	chip.addEventListener("click", () => selectWelcomeAgent(chip, agentId));
	return chip;
}

function appendHatchAgentButton(container: HTMLElement): void {
	const hatchButton = document.createElement("button");
	hatchButton.type = "button";
	hatchButton.className = "provider-btn provider-btn-secondary";
	hatchButton.style.fontSize = "0.7rem";
	hatchButton.style.padding = "3px 8px";
	hatchButton.textContent = "\u{1F95A} Hatch a new agent";
	hatchButton.addEventListener("click", () => navigate(settingsPath("agents/new")));
	container.appendChild(hatchButton);
}

function renderWelcomeAgentOptions(
	container: HTMLElement,
	agents: AgentInfo[],
	activeAgentId: string,
): AgentInfo | null {
	container.textContent = "";
	container.classList.remove("hidden");
	container.classList.add("flex");
	let activeAgent: AgentInfo | null = null;
	for (const agent of agents) {
		const agentId = agent?.id;
		if (!agentId) continue;
		if (agentId === activeAgentId) activeAgent = agent;
		container.appendChild(createWelcomeAgentChip(agent, agentId, activeAgentId));
	}
	appendHatchAgentButton(container);
	return activeAgent;
}

function handleWelcomeAgentsResponse(
	card: HTMLElement,
	container: HTMLElement,
	activeAgentId: string,
	onActiveAgentResolved: (agent: AgentInfo | null) => void,
	response: RpcResponse,
): void {
	if (!card.isConnected) return;
	if (!response.ok) {
		container.classList.add("hidden");
		return;
	}
	const parsed = parseAgentsListPayload(response.payload as Parameters<typeof parseAgentsListPayload>[0]);
	const agents = (parsed.agents || []) as AgentInfo[];
	const effectiveActive = activeAgentId || parsed.defaultId;
	onActiveAgentResolved(renderWelcomeAgentOptions(container, agents, effectiveActive));
}

export function renderWelcomeAgentPicker(
	card: HTMLElement,
	activeAgentId: string,
	onActiveAgentResolved: (agent: AgentInfo | null) => void,
): void {
	const container = card.querySelector("[data-welcome-agents]") as HTMLElement | null;
	if (!container) return;
	void sendRpc("agents.list", {}).then((response) =>
		handleWelcomeAgentsResponse(card, container, activeAgentId, onActiveAgentResolved, response),
	);
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
	const activeAgentId = sessionStore.activeSession.value?.agent_id || "";
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

interface HistoryRenderState {
	messageElements: (HTMLElement | null)[];
	pendingTerminalMetadata: Map<string, PendingTerminalToolMetadata>;
	latestToolContextBudget: ContextBudgetMetadata | null;
}

function registerAssistantTerminalMetadata(
	message: AssistantMsg,
	messageEl: HTMLElement | null,
	pendingMetadata: Map<string, PendingTerminalToolMetadata>,
): void {
	if (!isTerminalAssistantMessage(message)) return;
	const toolIds = toolCallIds(message.tool_calls);
	if (toolIds.length === 0) {
		appendTerminalMetadata(
			S.chatMsgBox,
			messageEl,
			terminalMetadataData(message, { historyIndex: message.historyIndex }),
		);
		return;
	}
	const pending: PendingTerminalToolMetadata = {
		message,
		remaining: new Set(toolIds),
		lastToolCard: null,
	};
	for (const toolCallId of toolIds) pendingMetadata.set(toolCallId, pending);
}

function resolvePendingToolMetadata(
	message: ToolResultMsg,
	toolCard: HTMLElement,
	pendingMetadata: Map<string, PendingTerminalToolMetadata>,
): void {
	const toolCallId = message.tool_call_id;
	if (!toolCallId) return;
	const pending = pendingMetadata.get(toolCallId);
	if (!pending) return;
	pending.remaining.delete(toolCallId);
	pending.lastToolCard = toolCard;
	if (pending.remaining.size > 0) return;
	for (const completedToolCallId of toolCallIds(pending.message.tool_calls)) {
		pendingMetadata.delete(completedToolCallId);
	}
	appendTerminalMetadata(
		S.chatMsgBox,
		toolCard,
		terminalMetadataData(pending.message, { historyIndex: pending.message.historyIndex }),
	);
}

function renderHistoryMessage(message: HistoryMessage, state: HistoryRenderState): void {
	switch (message.role) {
		case "user":
			state.messageElements.push(renderHistoryUserMessage(message as UserMsg));
			return;
		case "assistant": {
			const assistantMessage = message as AssistantMsg;
			const messageEl = renderHistoryAssistantMessage(assistantMessage);
			state.messageElements.push(messageEl);
			registerAssistantTerminalMetadata(assistantMessage, messageEl, state.pendingTerminalMetadata);
			return;
		}
		case "notice":
			state.messageElements.push(
				chatAddMsg("system", renderMarkdown(typeof message.content === "string" ? message.content : ""), true),
			);
			return;
		case "checkpoint": {
			const card = renderCheckpointCard(message as unknown as CheckpointHistoryMessage);
			if (card && typeof message.historyIndex === "number") card.dataset.historyIndex = String(message.historyIndex);
			state.messageElements.push(card);
			return;
		}
		case "tool_result": {
			const toolResult = message as ToolResultMsg;
			state.latestToolContextBudget = toolResult.contextBudget || null;
			const toolCard = renderHistoryToolResult(toolResult);
			state.messageElements.push(toolCard);
			resolvePendingToolMetadata(toolResult, toolCard, state.pendingTerminalMetadata);
			return;
		}
		default:
			state.messageElements.push(null);
	}
}

function appendRemainingTerminalMetadata(pendingMetadata: Map<string, PendingTerminalToolMetadata>): void {
	for (const pending of new Set(pendingMetadata.values())) {
		if (!pending.lastToolCard) continue;
		appendTerminalMetadata(
			S.chatMsgBox,
			pending.lastToolCard,
			terminalMetadataData(pending.message, { historyIndex: pending.message.historyIndex }),
		);
	}
}

function latestUserSequence(history: HistoryMessage[]): number {
	let maxSequence = 0;
	for (const message of history) {
		const sequence = (message as SeqHistoryMessage).seq;
		if (message.role === "user" && typeof sequence === "number" && sequence > maxSequence) maxSequence = sequence;
	}
	return maxSequence;
}

export function renderHistory(
	key: string,
	history: HistoryMessage[],
	searchContext: SearchContext | null,
	thinkingText: string | null,
	totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	hideSessionLoadIndicator();
	if (S.chatMsgBox) {
		S.chatMsgBox.classList.remove("chat-messages-empty");
		S.chatMsgBox.textContent = "";
	}
	S.setSessionTokens({ input: 0, output: 0 });
	S.setSessionCurrentInputTokens(0);
	S.setSessionCurrentContextTokens(0);
	S.setChatBatchLoading(true);
	const state: HistoryRenderState = {
		messageElements: [],
		pendingTerminalMetadata: new Map(),
		latestToolContextBudget: null,
	};
	for (const message of history) renderHistoryMessage(message, state);
	appendRemainingTerminalMetadata(state.pendingTerminalMetadata);
	updateTokenBar(state.latestToolContextBudget);
	S.setChatBatchLoading(false);
	if (S.chatMsgBox) highlightCodeBlocks(S.chatMsgBox);
	const historyTailIndex = computeHistoryTailIndex(history);
	syncHistoryState(key, history, historyTailIndex, totalCountHint);
	S.setChatSeq(latestUserSequence(history));
	if (history.length === 0) showWelcomeCard();
	postHistoryLoadActions(key, searchContext, state.messageElements, thinkingText, skipAutoScroll === true);
}
