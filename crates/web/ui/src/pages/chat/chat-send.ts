import { chatAddMsg, chatAddMsgWithAttachments } from "../../chat-ui";
import { highlightCodeBlocks } from "../../code-highlight";
import { renderMarkdown, sendRpc, warmAudioPlayback } from "../../helpers";
import {
	clearPendingAttachments,
	getPendingAttachments,
	hasPendingAttachments,
	type PendingAttachment,
	uploadDocumentAttachment,
} from "../../media-drop";
import { appendUserMessageCopyAction } from "../../message-actions";
import { selectedModelSelection } from "../../models";
import { handlePendingSendResponse, registerPendingSend, rejectPendingSend } from "../../sessions/pending-send";
import * as S from "../../state";
import type { ChatContentPart, ChatSendPayload, ChatSendRequest } from "../../types/chat";
import { handleSlashCommand, parseSlashCommand, shouldHandleSlashLocally, slashHideMenu } from "./slash-commands";

interface PendingImageAttachment extends PendingAttachment {
	dataUrl: string;
}

export function chatAutoResize(): void {
	if (!S.chatInput) return;
	S.chatInput.style.height = "auto";
	S.chatInput.style.height = `${Math.min(S.chatInput.scrollHeight, 120)}px`;
}

export function tryHandleLocalSlashCommand(text: string, hasAttachments: boolean): boolean {
	if (text.charAt(0) !== "/" || hasAttachments) return false;
	const slash = parseSlashCommand(text);
	if (!(slash && shouldHandleSlashLocally(slash.name))) return false;
	if (S.chatInput) S.chatInput.value = "";
	chatAutoResize();
	slashHideMenu();
	handleSlashCommand(slash.name, slash.args);
	return true;
}

export function handleHistoryUp(): void {
	if (S.chatHistory.length === 0 || !S.chatInput) return;
	if (S.chatHistoryIdx === -1) {
		S.setChatHistoryDraft(S.chatInput.value);
		S.setChatHistoryIdx(S.chatHistory.length - 1);
	} else if (S.chatHistoryIdx > 0) S.setChatHistoryIdx(S.chatHistoryIdx - 1);
	S.chatInput.value = S.chatHistory[S.chatHistoryIdx];
	chatAutoResize();
}

export function handleHistoryDown(): void {
	if (S.chatHistoryIdx === -1 || !S.chatInput) return;
	if (S.chatHistoryIdx < S.chatHistory.length - 1) {
		S.setChatHistoryIdx(S.chatHistoryIdx + 1);
		S.chatInput.value = S.chatHistory[S.chatHistoryIdx];
	} else {
		S.setChatHistoryIdx(-1);
		S.chatInput.value = S.chatHistoryDraft;
	}
	chatAutoResize();
}

export function rememberChatHistory(text: string): void {
	if (!text) return;
	S.chatHistory.push(text);
	if (S.chatHistory.length > 200) S.setChatHistory(S.chatHistory.slice(-200));
}

export function resetComposerAfterSend(): void {
	S.setChatHistoryIdx(-1);
	S.setChatHistoryDraft("");
	if (S.chatInput) S.chatInput.value = "";
	chatAutoResize();
	if (window.innerWidth < 768) S.chatInput?.blur();
}

async function buildChatMessage(text: string, sessionKey: string): Promise<{ params: ChatSendRequest; id: string }> {
	const attachments = hasPendingAttachments() ? getPendingAttachments() : [];
	const images = attachments.filter((attachment): attachment is PendingImageAttachment => Boolean(attachment.dataUrl));
	const documents = await Promise.all(
		attachments
			.filter((attachment) => !attachment.dataUrl)
			.map((attachment) => uploadDocumentAttachment(attachment, sessionKey)),
	);
	if (S.activeSessionKey !== sessionKey) throw new Error("Session changed while uploading attachments");
	const content: ChatContentPart[] = [];
	if (text) content.push({ type: "text", text });
	for (const image of images) content.push({ type: "image_url", image_url: { url: image.dataUrl } });
	const element =
		attachments.length > 0
			? chatAddMsgWithAttachments("user", renderMarkdown(text), images, documents)
			: chatAddMsg("user", renderMarkdown(text), true);
	appendUserMessageCopyAction(element, text);
	clearPendingAttachments();
	const id = registerPendingSend(sessionKey, element);
	const params: ChatSendRequest = content.length > 0 ? { content, clientMessageId: id } : { text, clientMessageId: id };
	if (documents.length > 0)
		params.documents = documents.map((document) => ({
			displayName: document.display_name,
			storedFilename: document.stored_filename,
			mimeType: document.mime_type,
			sizeBytes: document.size_bytes,
		}));
	if (element) void highlightCodeBlocks(element);
	return { params, id };
}

let maybeRefreshFullContextFn: (() => void) | null = null;
export function setMaybeRefreshFullContextFn(fn: () => void): void {
	maybeRefreshFullContextFn = fn;
}
let sendInProgress = false;
export function sendChat(): void {
	void sendChatAsync();
}

async function submitPending(sessionKey: string, id: string, params: ChatSendRequest): Promise<void> {
	try {
		const response = await sendRpc<ChatSendPayload>("chat.send", params);
		handlePendingSendResponse(sessionKey, id, response);
	} catch (error) {
		rejectPendingSend(sessionKey, id, error instanceof Error ? error.message : "Request failed");
	}
}

async function sendChatAsync(): Promise<void> {
	if (sendInProgress) return;
	const text = S.chatInput?.value.trim() || "";
	const hasAttachments = hasPendingAttachments();
	if (!((text || hasAttachments) && S.connected)) return;
	const sessionKey = S.activeSessionKey;
	sendInProgress = true;
	warmAudioPlayback();
	try {
		if (tryHandleLocalSlashCommand(text, hasAttachments)) return;
		const modelOverride = selectedModelSelection();
		if (!modelOverride) throw new Error("Select a model before sending a message");
		const message = await buildChatMessage(text, sessionKey);
		rememberChatHistory(text);
		resetComposerAfterSend();
		await submitPending(sessionKey, message.id, { ...message.params, modelOverride });
		if (sessionKey === S.activeSessionKey) maybeRefreshFullContextFn?.();
	} catch (error) {
		if (sessionKey === S.activeSessionKey)
			chatAddMsg("error", error instanceof Error ? error.message : "Request failed");
	} finally {
		sendInProgress = false;
	}
}
