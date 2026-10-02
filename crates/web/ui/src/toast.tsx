import type { Signal } from "@preact/signals";
import { signal } from "@preact/signals";
import type { VNode } from "preact";

interface Toast {
	id: number;
	message: string;
	type: string;
}

export const toasts: Signal<Toast[]> = signal([]);
let toastId = 0;

export function showToast(message: string, type: string = "info"): void {
	const id = ++toastId;
	toasts.value = toasts.value.concat([{ id: id, message: message, type: type }]);
	setTimeout(() => {
		toasts.value = toasts.value.filter((toast) => toast.id !== id);
	}, 4000);
}

export function Toasts(): VNode {
	return (
		<div class="skills-toast-container">
			{toasts.value.map((toast) => {
				const bg = toast.type === "error" ? "var(--error, #e55)" : "var(--accent)";
				return (
					<div
						key={toast.id}
						style={{
							pointerEvents: "auto",
							maxWidth: "420px",
							padding: "10px 16px",
							borderRadius: "6px",
							fontSize: ".8rem",
							fontWeight: 500,
							color: "#fff",
							background: bg,
							boxShadow: "0 4px 12px rgba(0,0,0,.15)",
						}}
					>
						{toast.message}
					</div>
				);
			})}
		</div>
	);
}
