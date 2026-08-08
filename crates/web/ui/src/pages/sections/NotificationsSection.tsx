// ── Notifications section ─────────────────────────────────────

import type { VNode } from "preact";
import { useEffect, useState } from "preact/hooks";
import { SectionHeading, StatusMessage, SubHeading } from "../../components/forms/SectionLayout";
import { onEvent } from "../../events";
import * as push from "../../push";
import { isStandalone } from "../../pwa";
import { rerender } from "./_shared";

interface PushSubscription {
	endpoint: string;
	device?: string;
	ip?: string;
	created_at?: string;
}

interface PushServerStatus {
	subscription_count?: number;
	subscriptions?: PushSubscription[];
}

function NotificationsShell({ children }: { children: VNode | VNode[] }): VNode {
	return (
		<div className="flex-1 flex flex-col min-w-0 p-4 gap-4 overflow-y-auto">
			<SectionHeading title="Notifications" />
			{children}
		</div>
	);
}

function NotificationsNotice({ title, detail }: { title: string; detail: VNode | string }): VNode {
	return (
		<NotificationsShell>
			<div
				style={{
					maxWidth: "600px",
					padding: "12px 16px",
					borderRadius: "6px",
					border: "1px solid var(--border)",
					background: "var(--surface)",
				}}
			>
				<p className="text-sm text-[var(--text)]" style={{ margin: 0 }}>
					{title}
				</p>
				<p className="text-xs text-[var(--muted)]" style={{ margin: "8px 0 0" }}>
					{detail}
				</p>
			</div>
		</NotificationsShell>
	);
}

function pushStatusText(needsInstall: boolean, subscribed: boolean, permission: string): string {
	if (needsInstall) return "Add this app to your Dock to enable notifications.";
	if (subscribed) return "You will receive notifications on this device.";
	if (permission === "denied") return "Notifications are blocked. Enable them in browser settings.";
	return "Enable to receive notifications on this device.";
}

function pushToggleLabel(toggling: boolean, subscribed: boolean): string {
	if (toggling) return "\u2026";
	return subscribed ? "Disable" : "Enable";
}

interface PushControlProps {
	needsInstall: boolean;
	subscribed: boolean;
	permission: string;
	toggling: boolean;
	error: string | null;
	onToggle: () => void;
}

function PushControl(props: PushControlProps): VNode {
	return (
		<div style={{ maxWidth: "600px" }}>
			<div className="provider-item" style={{ marginBottom: 0 }}>
				<div style={{ flex: 1, minWidth: 0 }}>
					<div className="provider-item-name" style={{ fontSize: ".9rem" }}>
						Push Notifications
					</div>
					<div style={{ fontSize: ".75rem", color: "var(--muted)", marginTop: "2px" }}>
						{pushStatusText(props.needsInstall, props.subscribed, props.permission)}
					</div>
				</div>
				<button
					type="button"
					className={`provider-btn ${props.subscribed ? "provider-btn-danger" : ""}`}
					onClick={props.onToggle}
					disabled={props.toggling || props.permission === "denied" || props.needsInstall}
				>
					{pushToggleLabel(props.toggling, props.subscribed)}
				</button>
			</div>
			<StatusMessage error={props.error} className="text-xs mt-2" />
		</div>
	);
}

function PushGuidance({ needsInstall, permission }: { needsInstall: boolean; permission: string }): VNode | null {
	if (needsInstall) {
		return (
			<div
				style={{
					maxWidth: "600px",
					padding: "12px 16px",
					borderRadius: "6px",
					border: "1px solid var(--border)",
					background: "var(--surface)",
				}}
			>
				<p className="text-sm text-[var(--text)]" style={{ margin: 0, fontWeight: 500 }}>
					Installation required
				</p>
				<p className="text-xs text-[var(--muted)]" style={{ margin: "8px 0 0" }}>
					On Safari, push notifications are only available for installed apps. Add chelix to your Dock using{" "}
					<strong>File {"\u2192"} Add to Dock</strong> (or Share {"\u2192"} Add to Dock on iOS), then open it from
					there.
				</p>
			</div>
		);
	}
	if (permission !== "denied") return null;
	return (
		<div
			style={{
				maxWidth: "600px",
				padding: "12px 16px",
				borderRadius: "6px",
				border: "1px solid var(--error)",
				background: "color-mix(in srgb, var(--error) 5%, transparent)",
			}}
		>
			<p className="text-sm" style={{ color: "var(--error)", margin: 0, fontWeight: 500 }}>
				Notifications are blocked
			</p>
			<p className="text-xs text-[var(--muted)]" style={{ margin: "8px 0 0" }}>
				You previously blocked notifications for this site. To enable them, you'll need to update your browser's site
				settings and allow notifications for this origin.
			</p>
		</div>
	);
}

function PushSubscriptionRow({
	subscription,
	onRemove,
}: {
	subscription: PushSubscription;
	onRemove: () => void;
}): VNode {
	return (
		<div className="provider-item" style={{ marginBottom: 0 }}>
			<div style={{ flex: 1, minWidth: 0 }}>
				<div className="provider-item-name" style={{ fontSize: ".85rem" }}>
					{subscription.device}
				</div>
				<div
					style={{
						fontSize: ".7rem",
						color: "var(--muted)",
						marginTop: "2px",
						display: "flex",
						gap: "12px",
						flexWrap: "wrap",
					}}
				>
					{subscription.ip && <span style={{ fontFamily: "var(--font-mono)" }}>{subscription.ip}</span>}
					<time dateTime={subscription.created_at}>{new Date(subscription.created_at || "").toLocaleDateString()}</time>
				</div>
			</div>
			<button type="button" className="provider-btn provider-btn-danger" onClick={onRemove}>
				Remove
			</button>
		</div>
	);
}

function SubscribedDevices({
	status,
	onRemove,
}: {
	status: PushServerStatus;
	onRemove: (endpoint: string) => void;
}): VNode {
	const subscriptions = status.subscriptions || [];
	return (
		<div style={{ maxWidth: "600px", borderTop: "1px solid var(--border)", paddingTop: "16px", marginTop: "8px" }}>
			<SubHeading title={`Subscribed Devices (${status.subscription_count || 0})`} />
			{subscriptions.length ? (
				<div style={{ display: "flex", flexDirection: "column", gap: "6px" }}>
					{subscriptions.map((subscription) => (
						<PushSubscriptionRow
							key={subscription.endpoint}
							subscription={subscription}
							onRemove={() => onRemove(subscription.endpoint)}
						/>
					))}
				</div>
			) : (
				<div className="text-xs text-[var(--muted)]" style={{ padding: "4px 0" }}>
					No devices subscribed yet.
				</div>
			)}
		</div>
	);
}

interface NotificationsContentProps extends PushControlProps {
	serverStatus: PushServerStatus;
	onRemoveSubscription: (endpoint: string) => void;
}

function NotificationsContent(props: NotificationsContentProps): VNode {
	return (
		<NotificationsShell>
			<p className="text-xs text-[var(--muted)] leading-relaxed" style={{ maxWidth: "600px", margin: 0 }}>
				Receive push notifications when the agent completes a task or needs your attention.
			</p>
			<PushControl {...props} />
			<PushGuidance needsInstall={props.needsInstall} permission={props.permission} />
			<SubscribedDevices status={props.serverStatus} onRemove={props.onRemoveSubscription} />
		</NotificationsShell>
	);
}

export function NotificationsSection(): VNode {
	const [supported, setSupported] = useState(false);
	const [permission, setPermission] = useState("default");
	const [subscribed, setSubscribed] = useState(false);
	const [isLoading, setIsLoading] = useState(true);
	const [toggling, setToggling] = useState(false);
	const [error, setError] = useState<string | null>(null);
	const [serverStatus, setServerStatus] = useState<PushServerStatus | null>(null);

	async function checkStatus(): Promise<void> {
		setIsLoading(true);
		rerender();

		const pushSupported = push.isPushSupported();
		setSupported(pushSupported);

		if (pushSupported) {
			setPermission(push.getPermissionState());
			await push.initPushState();
			setSubscribed(push.isSubscribed());

			const status = await push.getPushStatus();
			setServerStatus(status as PushServerStatus);
		}

		setIsLoading(false);
		rerender();
	}

	async function refreshStatus(): Promise<void> {
		const status = await push.getPushStatus();
		setServerStatus(status as PushServerStatus);
		rerender();
	}

	async function onRemoveSubscription(endpoint: string): Promise<void> {
		const result = await push.removeSubscription(endpoint);
		if (!result.success) {
			setError(result.error || "Failed to remove subscription");
			rerender();
		}
	}

	useEffect(() => {
		checkStatus();
		const off = onEvent("push.subscriptions", () => {
			refreshStatus();
		});
		return off;
	}, []);

	async function onToggle(): Promise<void> {
		setError(null);
		setToggling(true);
		rerender();

		const result = subscribed ? await push.unsubscribeFromPush() : await push.subscribeToPush();

		if (result.success) {
			setSubscribed(!subscribed);
			if (!subscribed) setPermission("granted");
		} else {
			setError(result.error || (subscribed ? "Failed to unsubscribe" : "Failed to subscribe"));
		}

		setToggling(false);
		rerender();
	}

	if (isLoading) {
		return (
			<NotificationsShell>
				<div className="text-xs text-[var(--muted)]">Loading{"\u2026"}</div>
			</NotificationsShell>
		);
	}
	if (!supported) {
		return (
			<NotificationsNotice
				title="Push notifications are not supported in this browser."
				detail="Try using Safari, Chrome, or Firefox on a device that supports web push."
			/>
		);
	}
	if (serverStatus === null) {
		return (
			<NotificationsNotice
				title="Push notifications are not configured on the server."
				detail={
					<>
						The server was built without the{" "}
						<code style={{ fontFamily: "var(--font-mono)", fontSize: ".75rem" }}>push-notifications</code> feature.
					</>
				}
			/>
		);
	}
	const needsInstall = !isStandalone() && /Safari/.test(navigator.userAgent) && !/Chrome/.test(navigator.userAgent);
	return (
		<NotificationsContent
			needsInstall={needsInstall}
			subscribed={subscribed}
			permission={permission}
			toggling={toggling}
			error={error}
			onToggle={onToggle}
			serverStatus={serverStatus}
			onRemoveSubscription={onRemoveSubscription}
		/>
	);
}
