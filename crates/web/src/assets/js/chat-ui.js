// E2E test compatibility shim.
//
// With Vite bundling, individual modules are no longer served. The real
// chat-ui module lives inside the bundle but is exposed on
// window.__chelix_modules["chat-ui"] from main.tsx.
//
// This shim re-exports everything the e2e tests need.

function chatUi() {
	return window.__chelix_modules?.["chat-ui"] || {};
}

export default new Proxy({}, {
	get(_target, prop) {
		return chatUi()[prop];
	},
});

export const chatAddMsg = (...args) => chatUi().chatAddMsg?.(...args);
export const chatAddMsgWithImages = (...args) => chatUi().chatAddMsgWithImages?.(...args);
export const updateTokenBar = (...args) => chatUi().updateTokenBar?.(...args);
export const renderApprovalCard = (...args) => chatUi().renderApprovalCard?.(...args);
export const updateCommandInputUI = (...args) => chatUi().updateCommandInputUI?.(...args);
export const smartScrollToBottom = (...args) => chatUi().smartScrollToBottom?.(...args);
