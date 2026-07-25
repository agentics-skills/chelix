const M = (window.__chelix_modules || {})["stores/session-store"] || {};
export default M;
export const sessionStore = M.sessionStore;
export const insertSessionInOrder = (...args) => M.insertSessionInOrder?.(...args);
