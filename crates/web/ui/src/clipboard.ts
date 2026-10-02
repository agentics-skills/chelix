import { showToast } from "./toast";

export async function copyToClipboard(
	text: string,
	successMessage = "Copied to clipboard",
	failMessage = "Could not copy — please copy manually.",
): Promise<boolean> {
	const onSuccess = () => {
		if (successMessage) showToast(successMessage, "success");
		return true;
	};
	const onFail = () => {
		if (failMessage) showToast(failMessage, "error");
		return false;
	};
	if (navigator.clipboard) {
		try {
			await navigator.clipboard.writeText(text);
			return onSuccess();
		} catch {
			// fall through to execCommand fallback
		}
	}
	try {
		const el = document.createElement("textarea");
		el.value = text;
		el.style.position = "fixed";
		el.style.opacity = "0";
		document.body.appendChild(el);
		try {
			el.select();
			const ok = document.execCommand("copy");
			return ok ? onSuccess() : onFail();
		} finally {
			document.body.removeChild(el);
		}
	} catch {
		return onFail();
	}
}
