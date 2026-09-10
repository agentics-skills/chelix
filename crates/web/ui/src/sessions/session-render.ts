// ── Session rendering: history messages, welcome card, session list ──

import {
	appendChannelFooter,
	appendReasoningDisclosure,
	chatAddErrorCard,
	chatAddMsg,
	chatAddMsgWithImages,
	highlightAndScroll,
	pinChatToBottom,
	preserveChatViewport,
	resetChatView,
	stripChannelPrefix,
	syncChatFollowStateFromPosition,
	updateTokenBar,
	withChatInsertionTarget,
} from "../chat-ui";
import { highlightCodeBlocks } from "../code-highlight";
import { unmountExecuteCommandToolBubbles } from "../components/ExecuteCommandToolBubble";
import * as gon from "../gon";
import { parseAgentsListPayload, renderAudioPlayer, renderDocument, renderMarkdown, sendRpc } from "../helpers";
import { appendMessageActions, appendUserMessageActions } from "../message-actions";
import { upsertTtsProviderFooter } from "../message-voice";
import { renderCheckpointCard } from "../pages/chat/context-card";
import { navigate } from "../router";
import { settingsPath } from "../routes";
import * as S from "../state";
import { modelStore } from "../stores/model-store";
import { getHistoryWindow } from "../stores/session-history-cache";
import { sessionStore } from "../stores/session-store";
import { appendTerminalMetadata, terminalMetadataData } from "../terminal-metadata";
import { terminalContextTokens } from "../terminal-usage";
import { isToolLifecycleEvent, reduceToolInvocation } from "../tool-lifecycle";
import type { RpcResponse } from "../types/rpc";
import type { HistoryMessage } from "../types/session";
import type { UiHistoryTarget, UiSnapshot } from "../types/ui-history";
import type {
	CheckpointHistoryMessage,
	ProviderOutputItem,
	ReasoningContent,
	ToolLifecycleEvent,
} from "../types/ws-events";
import { hasVisibleReasoning } from "../types/ws-events";
import { showToast } from "../ui";
import { setSafeMarkdownHtml } from "../ws/shared";
import { renderToolLifecycleSnapshot } from "../ws/tool-helpers";
import { confirmPendingSend } from "./pending-send";

import { extractSegmentReasoning, segmentFromItems } from "./provider-segment-reducer";
import { setSessionAgent } from "./session-agent";
import { syncHistoryState } from "./session-history";
import { fetchSessions } from "./session-list";

// ── Types ────────────────────────────────────────────────────

export interface SearchContext {
	query: string;
	messageId: string;
	generation: string;
}

type ToolLifecycleHistoryMessage = HistoryMessage &
	ToolLifecycleEvent & {
		accumulatedArguments?: string;
	};

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
	reasoning?: ReasoningContent;
	audio?: string;
	tts_provider?: string;
	run_id?: string;
	providerItems?: ProviderOutputItem[];
	segmentId?: string;
	requestInputTokens?: number;
	requestOutputTokens?: number;
	requestCacheReadTokens?: number;
	requestCacheWriteTokens?: number;
	tool_calls?: unknown[];
	created_at?: number;
}

type HistoryMessageIdentity = Pick<HistoryMessage, "canonicalCommitted" | "id" | "generation">;

interface UserMsg extends HistoryMessageIdentity, Omit<HistoryMessage, "content"> {
	content?: string | unknown[];
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

interface AgentInfo {
	id: string;
	name: string;
	emoji?: string | null;
	model: string;
	reasoning_effort: string;
}

function isAgentsListPayload(value: unknown): value is Parameters<typeof parseAgentsListPayload>[0] {
	if (typeof value !== "object" || value === null) return false;
	const record = value as Record<string, unknown>;
	return Array.isArray(record.agents) && typeof record.default_id === "string";
}

function toAgentInfo(value: unknown): AgentInfo | null {
	if (typeof value !== "object" || value === null) return null;
	const record = value as Record<string, unknown>;
	const { id, name, emoji, model, reasoning_effort: reasoningEffort } = record;
	if (
		!(
			typeof id === "string" &&
			id.trim().length > 0 &&
			typeof name === "string" &&
			name.trim().length > 0 &&
			(emoji === undefined || emoji === null || typeof emoji === "string") &&
			typeof model === "string" &&
			model.trim().length > 0 &&
			typeof reasoningEffort === "string" &&
			reasoningEffort.trim().length > 0
		)
	)
		return null;
	return { id, name, emoji, model, reasoning_effort: reasoningEffort };
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
	const messageEl =
		confirmPendingSend(S.activeSessionKey, msg.clientMessageId) || renderUserMessageBody(msg, text, images);
	if (messageEl && messageEl.parentElement !== currentMessageContainer) currentMessageContainer?.appendChild(messageEl);
	appendUserDocuments(messageEl, msg.documents);
	appendUserMessageActions({
		messageEl,
		sessionKey: S.activeSessionKey,
		target: historyTarget(msg),
		text,
	});
	if (messageEl && msg.channel) appendChannelFooter(messageEl, msg.channel);
	return messageEl;
}

function isTerminalAssistantMessage(msg: AssistantMsg): boolean {
	return msg.durationMs !== undefined || !Array.isArray(msg.tool_calls) || msg.tool_calls.length === 0;
}

function hasVisibleAssistantContent(msg: AssistantMsg): boolean {
	return Boolean(msg.content?.trim() || hasVisibleReasoning(msg.reasoning) || msg.audio);
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
	if (msg.audio) return renderAssistantAudioMessage(msg);
	const messageEl = chatAddMsg("assistant", "");
	if (messageEl) {
		updateAssistantText(messageEl, msg.content || "");
		appendAssistantReasoning(messageEl, msg);
	}
	return messageEl;
}

/// Render reasoning strictly by canonical provider item position. Falls back to
/// the persisted reasoning field only when no provider items exist.
function appendAssistantReasoning(messageEl: HTMLElement, msg: AssistantMsg): void {
	const expanded = messageEl.querySelector<HTMLDetailsElement>(".msg-reasoning")?.open ?? false;
	const streaming = msg.outcome === "active";
	const providerItems = Array.isArray(msg.providerItems) ? msg.providerItems : [];
	if (providerItems.length === 0) {
		if (msg.reasoning) {
			appendReasoningDisclosure(messageEl, msg.reasoning, { expanded, streaming });
		}
		return;
	}
	// Every reasoning item of the segment belongs to the same disclosure: they
	// are parts of one reasoning stream, and the live view renders them the same
	// way. One disclosure per item would show the message thinking several times.
	const segment = segmentFromItems(msg.segmentId ?? "", providerItems);
	const reasoning = extractSegmentReasoning(segment);
	if (hasVisibleReasoning(reasoning)) {
		appendReasoningDisclosure(messageEl, reasoning, { expanded, streaming });
	}
}

function decorateAssistantMessage(messageEl: HTMLElement | null, msg: AssistantMsg): void {
	if (!messageEl) return;
	upsertTtsProviderFooter(messageEl, msg.tts_provider);
	appendMessageActions({
		messageEl,
		sessionKey: S.activeSessionKey,
		target: historyTarget(msg),
		text: msg.content || "",
		hasAudio: Boolean(msg.audio),
	});
}

function renderHistoryAssistantMessage(msg: AssistantMsg, applySessionUsage: boolean): HTMLElement | null {
	const isTerminal = isTerminalAssistantMessage(msg) && applySessionUsage;
	if (!hasVisibleAssistantContent(msg)) {
		applyTerminalAssistantUsage(msg, isTerminal);
		return null;
	}
	const messageEl = renderAssistantMessageBody(msg);
	decorateAssistantMessage(messageEl, msg);
	applyTerminalAssistantUsage(msg, isTerminal);
	return messageEl;
}

function makeThinkingDots(): HTMLElement {
	const template = S.$<HTMLTemplateElement>("tpl-thinking-dots");
	if (!template) throw new Error("Thinking dots template is missing");
	const element = (template.content.cloneNode(true) as DocumentFragment).firstElementChild;
	if (!(element instanceof HTMLElement)) throw new Error("Thinking dots template is empty");
	return element;
}

function refreshHistoryContext(): void {
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
}

function scrollAfterHistoryLoad(
	searchContext: SearchContext | null,
	msgEls: (HTMLElement | null)[],
	skipAutoScroll: boolean,
): void {
	if (!skipAutoScroll && searchContext?.query && S.chatMsgBox) {
		// A search jump is not follow mode: late decoration must not drag the
		// viewport away from the highlighted match.
		syncChatFollowStateFromPosition();
		highlightAndScroll(msgEls, searchContext.messageId, searchContext.query);
		return;
	}
	if (skipAutoScroll) {
		syncChatFollowStateFromPosition();
		return;
	}
	// The bottom must be established synchronously: the headroom fill that runs
	// right after this render measures `scrollTop`, and a scroll deferred to an
	// animation frame would make it see the top of a freshly reset view.
	pinChatToBottom(true);
}

export function postHistoryLoadActions(
	key: string,
	searchContext: SearchContext | null,
	msgEls: (HTMLElement | null)[],
	skipAutoScroll: boolean,
): void {
	refreshHistoryContext();
	scrollAfterHistoryLoad(searchContext, msgEls, skipAutoScroll);
	if (key !== S.activeSessionKey) return;
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
	chip.textContent = `${labelPrefix}${agent.name}`;
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
		if (agent.id === activeAgentId) activeAgent = agent;
		container.appendChild(createWelcomeAgentChip(agent, agent.id, activeAgentId));
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
	const payload = response.payload;
	if (!isAgentsListPayload(payload)) {
		container.textContent = "";
		container.classList.add("hidden");
		container.classList.remove("flex");
		onActiveAgentResolved(null);
		return;
	}
	const parsed = parseAgentsListPayload(payload);
	const agents: AgentInfo[] = [];
	for (const entry of parsed.agents) {
		const agent = toAgentInfo(entry);
		if (!agent) {
			container.textContent = "";
			container.classList.add("hidden");
			container.classList.remove("flex");
			onActiveAgentResolved(null);
			return;
		}
		agents.push(agent);
	}
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

let renderedKey = "";
let renderedGeneration = "";
let currentMessageContainer: HTMLElement | null = null;

function historyTarget(message: HistoryMessageIdentity): UiHistoryTarget | undefined {
	return message.canonicalCommitted && message.id && message.generation
		? { messageId: message.id, generation: message.generation }
		: undefined;
}

function renderToolSnapshot(message: UiSnapshot): void {
	if (!isToolLifecycleEvent(message)) throw new Error("Invalid UI tool snapshot");
	const lifecycle = message as ToolLifecycleHistoryMessage;
	const snapshot = reduceToolInvocation(undefined, lifecycle, {
		runId: lifecycle.runId,
		contextBudget: lifecycle.contextBudget,
		accumulatedArguments: lifecycle.accumulatedArguments,
		executionMode:
			typeof message.presentation.metadata?.executionMode === "string"
				? message.presentation.metadata.executionMode
				: undefined,
	});
	renderToolLifecycleSnapshot(snapshot, S.activeSessionKey, {
		interactive: true,
		screenshotMode: "media",
		assistantId: message.assistantId,
	});
}

function updateAssistantText(element: HTMLElement, text: string): void {
	let body = element.querySelector<HTMLElement>(":scope > .assistant-text");
	if (!body) {
		body = document.createElement("div");
		body.className = "assistant-text";
		element.appendChild(body);
	}
	setSafeMarkdownHtml(body, text);
}

function renderAssistantSnapshot(message: UiSnapshot, container: HTMLElement): void {
	const assistant = message as AssistantMsg;
	let element = container.querySelector<HTMLElement>(":scope > .msg.assistant");
	if (element && !assistant.audio) {
		updateAssistantText(element, assistant.content || "");
		appendAssistantReasoning(element, assistant);
		decorateAssistantMessage(element, assistant);
	} else if (!element || element.dataset.audio !== assistant.audio) {
		container.replaceChildren();
		element = renderHistoryAssistantMessage(assistant, false);
		if (element && assistant.audio) element.dataset.audio = assistant.audio;
	}
	const medium = message.presentation.metadata?.replyMedium;
	appendTerminalMetadata(
		container,
		element,
		terminalMetadataData(assistant, {
			messageId: message.id,
			replyMedium: typeof medium === "string" ? medium : undefined,
		}),
	);
	let warning = container.querySelector<HTMLElement>(":scope > .audio-warning");
	const audioWarning = message.presentation.metadata?.audioWarning;
	if (typeof audioWarning === "string") {
		if (!warning) {
			warning = document.createElement("div");
			warning.className = "audio-warning text-sm text-amber-400";
			container.appendChild(warning);
		}
		warning.textContent = audioWarning;
	} else warning?.remove();
}

function renderUserSnapshot(message: UiSnapshot, container: HTMLElement): void {
	const element = container.querySelector<HTMLElement>(":scope > .msg.user");
	if (!element) {
		renderHistoryUserMessage(message as UserMsg);
		return;
	}
	appendUserMessageActions({
		messageEl: element,
		sessionKey: S.activeSessionKey,
		text: userMessageContent(message as UserMsg).text,
		target: historyTarget(message),
	});
}

function renderSnapshotBody(message: UiSnapshot, container: HTMLElement): void {
	switch (message.role) {
		case "tool_lifecycle":
			renderToolSnapshot(message);
			return;
		case "assistant":
			renderAssistantSnapshot(message, container);
			return;
		case "user":
			renderUserSnapshot(message, container);
			return;
		case "checkpoint":
			container.replaceChildren();
			renderCheckpointCard(message as unknown as CheckpointHistoryMessage);
			return;
		case "error":
			renderProviderError(message, container);
			return;
		default:
			container.replaceChildren();
			chatAddMsg("system", renderMarkdown(typeof message.content === "string" ? message.content : ""), true);
	}
}

function renderProviderError(message: UiSnapshot, container: HTMLElement): void {
	const error = message.error as {
		raw: string;
		retryAfterMs?: number | null;
		details: { title?: string; detail?: string; provider?: string; icon?: string };
	};
	if (!error || typeof error.raw !== "string") throw new Error("Invalid provider error snapshot");
	container.replaceChildren();
	chatAddErrorCard({ ...error.details, title: error.details.title || error.raw });
	const raw = document.createElement("details");
	const label = document.createElement("summary");
	label.textContent = "Provider error";
	const text = document.createElement("pre");
	text.className = "whitespace-pre-wrap break-words";
	text.textContent = error.raw;
	raw.append(label, text);
	container.appendChild(raw);
	if (typeof error.retryAfterMs === "number") {
		const retry = document.createElement("div");
		retry.textContent = `Retry delay: ${error.retryAfterMs} ms`;
		container.appendChild(retry);
	}
}

function renderDiff(presentation: HTMLElement, content: string): void {
	const pre = document.createElement("pre");
	pre.className = "whitespace-pre-wrap break-words";
	for (const line of content.split("\n")) {
		const row = document.createElement("div");
		if (line.startsWith("+")) row.className = "text-emerald-400";
		else if (line.startsWith("-")) row.className = "text-red-400";
		row.textContent = line || " ";
		pre.appendChild(row);
	}
	presentation.replaceChildren(pre);
}

function renderPresentation(message: UiSnapshot, container: HTMLElement): void {
	const presentationDocument = message.presentation.document;
	for (const child of Array.from(container.children)) {
		if (
			!(child instanceof HTMLElement) ||
			child.classList.contains("ui-presentation") ||
			child.classList.contains("terminal-metadata")
		)
			continue;
		child.classList.toggle("hidden", !!presentationDocument);
	}
	let presentation = container.querySelector<HTMLElement>(":scope > .ui-presentation");
	if (!presentationDocument) {
		presentation?.remove();
		return;
	}
	if (!presentation) {
		presentation = document.createElement("div");
		presentation.className = "ui-presentation";
		container.appendChild(presentation);
	}
	if (presentationDocument.format === "markdown") setSafeMarkdownHtml(presentation, presentationDocument.content);
	else if (presentationDocument.format === "diff") renderDiff(presentation, presentationDocument.content);
	else presentation.textContent = presentationDocument.content;
}

function renderSnapshot(message: UiSnapshot, container: HTMLElement): void {
	currentMessageContainer = container;
	try {
		withChatInsertionTarget(container, () => {
			renderSnapshotBody(message, container);
			renderPresentation(message, container);
		});
	} finally {
		currentMessageContainer = null;
	}
	container.dataset.revision = String(message.revision);
}

function retainedMessageNodes(box: HTMLElement, history: UiSnapshot[]): Map<string, HTMLElement> {
	const nodes = new Map<string, HTMLElement>();
	const retained = new Set(history.map((message) => message.id));
	for (const child of Array.from(box.children)) {
		if (!(child instanceof HTMLElement && child.dataset.messageId)) continue;
		if (retained.has(child.dataset.messageId)) nodes.set(child.dataset.messageId, child);
		else {
			unmountExecuteCommandToolBubbles(child);
			child.remove();
		}
	}
	return nodes;
}

function reconcileNodes(box: HTMLElement, history: UiSnapshot[]): void {
	box.querySelector("#welcomeCard")?.remove();
	box.querySelector("#noProvidersCard")?.remove();
	box.classList.remove("chat-messages-empty");
	const nodes = retainedMessageNodes(box, history);
	S.setChatBatchLoading(true);
	try {
		let previous: HTMLElement | null = null;
		for (const message of history) {
			let node = nodes.get(message.id);
			if (!node) {
				node = document.createElement("div");
				node.className = "flex flex-col gap-2";
				node.dataset.messageId = message.id;
			}
			const before: ChildNode | null = previous ? previous.nextSibling : box.firstChild;
			if (before !== node) box.insertBefore(node, before);
			if (node.dataset.revision !== String(message.revision)) renderSnapshot(message, node);
			previous = node;
		}
	} finally {
		S.setChatBatchLoading(false);
	}
}

function retainedViewportAnchor(box: HTMLElement, history: UiSnapshot[]): HTMLElement | null {
	const retained = new Set(history.map((message) => message.id));
	const viewport = box.getBoundingClientRect();
	for (const child of box.children) {
		if (!(child instanceof HTMLElement && child.dataset.messageId && retained.has(child.dataset.messageId))) continue;
		const bounds = child.getBoundingClientRect();
		if (bounds.bottom > viewport.top && bounds.top < viewport.bottom) return child;
	}
	return null;
}

export function reconcileSessionHistory(key: string): void {
	const box = S.chatMsgBox;
	const historyWindow = getHistoryWindow(key);
	if (!(box && historyWindow && key === S.activeSessionKey)) return;
	hideSessionLoadIndicator();
	if (renderedKey !== key || renderedGeneration !== historyWindow.generation) {
		resetChatView(box);
		renderedKey = key;
		renderedGeneration = historyWindow.generation;
	}
	const anchor = retainedViewportAnchor(box, historyWindow.history);
	preserveChatViewport(box, anchor, () => reconcileNodes(box, historyWindow.history));
	syncHistoryState(key);
	if (historyWindow.history.length === 0) showWelcomeCard();
	pinChatToBottom();
}

export function renderHistory(
	key: string,
	history: UiSnapshot[],
	searchContext: SearchContext | null,
	_totalCountHint: number | null,
	skipAutoScroll: boolean,
): void {
	reconcileSessionHistory(key);
	const elements = history.map(
		(message) => S.chatMsgBox?.querySelector<HTMLElement>(`[data-message-id="${CSS.escape(message.id)}"]`) || null,
	);
	postHistoryLoadActions(key, searchContext, elements, skipAutoScroll);
	if (S.chatMsgBox) void highlightCodeBlocks(S.chatMsgBox);
}
