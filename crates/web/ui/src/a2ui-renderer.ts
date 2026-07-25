import { A2uiSurface, basicCatalog, type LitComponentApi } from "@a2ui/lit/v0_9";
import {
	A2uiClientMessageSchema,
	A2uiMessageSchema,
	MessageProcessor,
	type A2uiClientAction,
	type A2uiMessage,
} from "@a2ui/web_core/v0_9";

import { sendRpc } from "./helpers";
import { getToolCardDetailsContainer, setToolCardStatus } from "./tool-call-card";

export const A2UI_TOOL_NAME = "render_a2ui";
export const A2UI_PROTOCOL_VERSION = "v0.9.1";
export const A2UI_BASIC_CATALOG_ID = "https://a2ui.org/specification/v0_9/catalogs/basic/catalog.json";

const MAX_PAYLOAD_BYTES = 128 * 1024;
const MAX_MESSAGES = 64;
const MAX_COMPONENTS = 200;

interface A2uiActionMessage {
	version: typeof A2UI_PROTOCOL_VERSION;
	action: A2uiClientAction;
}

interface ParsedToolArguments {
	messages: A2uiMessage[];
	surfaceId: string;
}

export interface MountA2uiCardOptions {
	arguments: unknown;
	runId?: string;
	toolCallId?: string;
	interactive: boolean;
	success?: boolean;
	result?: unknown;
	error?: string;
}

interface A2uiCardController {
	processor: MessageProcessor<LitComponentApi>;
	surface: A2uiSurface;
	feedback: HTMLElement;
	surfaceId: string;
	runId?: string;
	toolCallId?: string;
	state: "waiting" | "submitting" | "submitted" | "completed" | "error";
}

const controllers = new WeakMap<HTMLElement, A2uiCardController>();

export function isA2uiTool(toolName: string | undefined): boolean {
	return toolName === A2UI_TOOL_NAME;
}

function isRecord(value: unknown): value is Record<string, unknown> {
	return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function payloadByteLength(value: unknown): number {
	let serialized: string;
	try {
		serialized = JSON.stringify(value);
	} catch (error) {
		throw new Error(`A2UI payload is not JSON-serializable: ${error instanceof Error ? error.message : String(error)}`);
	}
	return new TextEncoder().encode(serialized).byteLength;
}

function parseToolArguments(value: unknown): ParsedToolArguments {
	if (!isRecord(value)) throw new Error("A2UI tool arguments must be an object.");
	if (payloadByteLength(value) > MAX_PAYLOAD_BYTES) {
		throw new Error(`A2UI payload exceeds ${MAX_PAYLOAD_BYTES} bytes.`);
	}
	if (!Array.isArray(value.messages) || value.messages.length === 0 || value.messages.length > MAX_MESSAGES) {
		throw new Error(`A2UI messages must contain between 1 and ${MAX_MESSAGES} entries.`);
	}

	const messages = value.messages.map((message, index) => {
		const parsed = A2uiMessageSchema.safeParse(message);
		if (!parsed.success) {
			const detail = parsed.error.issues[0]?.message || "schema validation failed";
			throw new Error(`A2UI message ${index + 1} is invalid: ${detail}.`);
		}
		if (parsed.data.version !== A2UI_PROTOCOL_VERSION) {
			throw new Error(`A2UI message ${index + 1} uses ${parsed.data.version}; expected ${A2UI_PROTOCOL_VERSION}.`);
		}
		return parsed.data;
	});

	const first = messages[0];
	if (!("createSurface" in first)) throw new Error("The first A2UI message must be createSurface.");
	if (first.createSurface.catalogId !== A2UI_BASIC_CATALOG_ID) {
		throw new Error(`Unsupported A2UI catalog: ${first.createSurface.catalogId}.`);
	}
	const surfaceId = first.createSurface.surfaceId;
	let componentCount = 0;
	for (const message of messages) {
		if ("createSurface" in message && message.createSurface.surfaceId !== surfaceId) {
			throw new Error("A2UI messages target different surfaces.");
		}
		if ("updateComponents" in message) {
			if (message.updateComponents.surfaceId !== surfaceId) {
				throw new Error("A2UI messages target different surfaces.");
			}
			componentCount += message.updateComponents.components.length;
		}
		if ("updateDataModel" in message && message.updateDataModel.surfaceId !== surfaceId) {
			throw new Error("A2UI messages target different surfaces.");
		}
		if ("deleteSurface" in message) throw new Error("Interactive A2UI payloads cannot delete their surface.");
	}
	if (componentCount === 0 || componentCount > MAX_COMPONENTS) {
		throw new Error(`A2UI payload must contain between 1 and ${MAX_COMPONENTS} components.`);
	}

	return { messages, surfaceId };
}

export function getA2uiToolArgumentsError(value: unknown): string | null {
	try {
		parseToolArguments(value);
		return null;
	} catch (error) {
		return error instanceof Error ? error.message : String(error);
	}
}

function parseActionMessage(value: unknown): A2uiActionMessage {
	let candidate = value;
	if (typeof candidate === "string") {
		try {
			candidate = JSON.parse(candidate) as unknown;
		} catch (error) {
			throw new Error(`A2UI result is not valid JSON: ${error instanceof Error ? error.message : String(error)}`);
		}
	}
	const parsed = A2uiClientMessageSchema.safeParse(candidate);
	if (!parsed.success || !("action" in parsed.data)) {
		throw new Error("A2UI result is not a standard client action message.");
	}
	if (parsed.data.version !== A2UI_PROTOCOL_VERSION) {
		throw new Error(`A2UI result uses ${parsed.data.version}; expected ${A2UI_PROTOCOL_VERSION}.`);
	}
	return {
		version: A2UI_PROTOCOL_VERSION,
		action: parsed.data.action,
	};
}

/** Insert the A2UI section above the standard Result section so the compact
 * Parameters, Result, and Context budget disclosures stay untouched. */
function insertA2uiSection(card: HTMLElement): HTMLElement {
	card.classList.add("a2ui-tool-card");
	card.querySelector("[data-a2ui-section]")?.remove();

	const details = getToolCardDetailsContainer(card);
	const section = document.createElement("section");
	section.className = "tool-call-section a2ui-section";
	section.setAttribute("data-a2ui-section", "");

	const title = document.createElement("div");
	title.className = "tool-call-section-title";
	title.textContent = `Interface \u00b7 A2UI ${A2UI_PROTOCOL_VERSION}`;
	section.appendChild(title);

	const resultSection = details.querySelector(".tool-call-result-section");
	if (resultSection) details.insertBefore(section, resultSection);
	else details.appendChild(section);
	return section;
}

function makeCardBody(card: HTMLElement): { surface: A2uiSurface; feedback: HTMLElement } {
	const section = insertA2uiSection(card);

	const surface = document.createElement("a2ui-surface");
	if (!(surface instanceof A2uiSurface)) throw new Error("The official A2UI Lit surface failed to register.");
	surface.className = "a2ui-surface-shell";
	section.appendChild(surface);

	const feedback = document.createElement("div");
	feedback.className = "a2ui-feedback";
	feedback.setAttribute("aria-live", "polite");
	section.appendChild(feedback);

	return { surface, feedback };
}

function setFeedback(controller: A2uiCardController, message: string, error = false): void {
	controller.feedback.textContent = message;
	controller.feedback.classList.toggle("a2ui-feedback-error", error);
	controller.feedback.setAttribute("role", error ? "alert" : "status");
}

function setLocked(controller: A2uiCardController, locked: boolean): void {
	controller.surface.inert = locked;
	controller.surface.classList.toggle("a2ui-surface-locked", locked);
	controller.surface.setAttribute("aria-disabled", String(locked));
	if (locked) closeOpenDialogs(controller.surface);
}

/** A locked surface is `inert`, so an open `<dialog>` rendered by the official
 * Modal component would trap the page behind an unusable overlay that can only
 * be dismissed by reloading. Close every open dialog when the surface locks. */
function closeOpenDialogs(root: Element | ShadowRoot): void {
	// Every catalog component renders into its own shadow root, and the surface
	// element itself has one, so a light-DOM query on the surface finds nothing.
	// Descend through each shadow root, including the starting element's.
	if (root instanceof Element && root.shadowRoot) closeOpenDialogs(root.shadowRoot);
	for (const dialog of root.querySelectorAll<HTMLDialogElement>("dialog[open]")) {
		dialog.close();
	}
	for (const element of root.querySelectorAll("*")) {
		if (element.shadowRoot) closeOpenDialogs(element.shadowRoot);
	}
}

function failController(card: HTMLElement, controller: A2uiCardController, message: string): void {
	controller.state = "error";
	setLocked(controller, true);
	setFeedback(controller, message, true);
	setToolCardStatus(card, "error", "interface error");
}

async function submitAction(card: HTMLElement, controller: A2uiCardController, action: A2uiClientAction): Promise<void> {
	if (controller.state !== "waiting") return;
	if (!(controller.runId && controller.toolCallId)) {
		failController(card, controller, "This A2UI interaction is missing its trusted run identifiers.");
		return;
	}
	if (action.surfaceId !== controller.surfaceId) {
		failController(card, controller, "The A2UI renderer emitted an action for a different surface.");
		return;
	}

	const standardMessage = {
		version: A2UI_PROTOCOL_VERSION,
		action,
	};
	const validated = A2uiClientMessageSchema.safeParse(standardMessage);
	if (!validated.success || !("action" in validated.data)) {
		failController(card, controller, "The A2UI renderer emitted an invalid standard action.");
		return;
	}

	controller.state = "submitting";
	setLocked(controller, true);
	setFeedback(controller, "Sending your response\u2026");
	setToolCardStatus(card, "running", "submitting\u2026");
	card.setAttribute("aria-busy", "true");

	const response = await sendRpc<{ accepted: boolean }>("a2ui.action", {
		runId: controller.runId,
		toolCallId: controller.toolCallId,
		message: validated.data,
	});
	card.removeAttribute("aria-busy");
	if (!(response.ok && response.payload?.accepted === true)) {
		controller.state = "waiting";
		setLocked(controller, false);
		setFeedback(controller, response.error?.message || "The A2UI response was not accepted.", true);
		setToolCardStatus(card, "retry", "send failed \u2014 retry");
		return;
	}

	controller.state = "submitted";
	setFeedback(controller, "Response sent. Waiting for the agent\u2026");
	setToolCardStatus(card, "running", "response sent\u2026");
}

/** Reflect the terminal tool outcome inside the surface section. The card
 * status and the standard result disclosures are owned by the tool card. */
function applyCompletion(
	card: HTMLElement,
	controller: A2uiCardController,
	success: boolean,
	result: unknown,
	error?: string,
): void {
	setLocked(controller, true);
	card.removeAttribute("aria-busy");
	if (!success) {
		controller.state = "error";
		setFeedback(controller, error || "The A2UI interaction failed without an error message.", true);
		return;
	}
	try {
		const message = parseActionMessage(result);
		if (message.action.surfaceId !== controller.surfaceId) {
			throw new Error("The completed action belongs to a different A2UI surface.");
		}
		controller.state = "completed";
		setFeedback(controller, `Response recorded: ${message.action.name}.`);
	} catch (completionError) {
		controller.state = "error";
		setFeedback(controller, completionError instanceof Error ? completionError.message : String(completionError), true);
	}
}

/** Report a surface that could not be mounted without discarding the standard
 * Parameters, Result, and Context budget disclosures. */
function renderMountFailure(card: HTMLElement, message: string): void {
	const section = insertA2uiSection(card);
	const alert = document.createElement("div");
	alert.className = "a2ui-mount-error";
	alert.setAttribute("role", "alert");
	alert.textContent = `Unable to render A2UI: ${message}`;
	section.appendChild(alert);
}

export function mountA2uiToolCard(card: HTMLElement, options: MountA2uiCardOptions): void {
	const existing = controllers.get(card);
	if (existing) {
		if (options.runId) existing.runId = options.runId;
		if (options.toolCallId) existing.toolCallId = options.toolCallId;
		if (options.success !== undefined) {
			applyCompletion(card, existing, options.success, options.result, options.error);
		}
		return;
	}

	let parsed: ParsedToolArguments;
	try {
		parsed = parseToolArguments(options.arguments);
	} catch (error) {
		renderMountFailure(card, error instanceof Error ? error.message : String(error));
		return;
	}

	try {
		const { surface, feedback } = makeCardBody(card);
		let controller: A2uiCardController | undefined;
		const processor = new MessageProcessor(
			[basicCatalog],
			(action) => {
				if (!controller) return;
				return submitAction(card, controller, action);
			},
			{ version: A2UI_PROTOCOL_VERSION },
		);
		controller = {
			processor,
			surface,
			feedback,
			surfaceId: parsed.surfaceId,
			runId: options.runId,
			toolCallId: options.toolCallId,
			state: options.interactive ? "waiting" : "completed",
		};
		controllers.set(card, controller);

		let created = false;
		processor.onSurfaceCreated((surfaceModel) => {
			if (!controller || surfaceModel.id !== controller.surfaceId) return;
			created = true;
			controller.surface.surface = surfaceModel;
			surfaceModel.onError.subscribe((surfaceError: unknown) => {
				if (!controller) return;
				failController(
					card,
					controller,
					`A2UI renderer error: ${surfaceError instanceof Error ? surfaceError.message : String(surfaceError)}`,
				);
			});
		});
		processor.processMessages(parsed.messages);
		if (!created) throw new Error(`A2UI did not create the declared surface ${parsed.surfaceId}.`);

		if (options.interactive) {
			setLocked(controller, false);
			setFeedback(controller, "Waiting for your response.");
			setToolCardStatus(card, "running", "waiting for response\u2026");
		} else {
			setLocked(controller, true);
			applyCompletion(card, controller, options.success === true, options.result, options.error);
		}
	} catch (error) {
		controllers.delete(card);
		renderMountFailure(card, error instanceof Error ? error.message : String(error));
	}
}

export function completeA2uiToolCard(card: HTMLElement, success: boolean, result: unknown, error?: string): void {
	const controller = controllers.get(card);
	if (!controller) {
		renderMountFailure(card, "The A2UI surface was not initialized before completion.");
		return;
	}
	applyCompletion(card, controller, success, result, error);
}
