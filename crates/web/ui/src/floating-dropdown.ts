// ── Shared floating dropdown positioning ─────────────────────

interface FloatingDropdownOptions {
	minWidth?: number;
	viewportPadding?: number;
	zIndex?: number;
	openUpThreshold?: number;
	minHeight?: number;
}

/**
 * Position dropdowns outside scroll-clipped toolbar rows.
 *
 * Dropdowns are fixed to the viewport, clamped horizontally, and can open
 * upward when there is not enough room below the anchor.
 */
export function positionFloatingDropdown(
	dropdown: HTMLElement,
	anchor: HTMLElement,
	options: FloatingDropdownOptions = {},
): void {
	const viewportPadding = options.viewportPadding ?? 8;
	const minWidth = options.minWidth ?? 200;
	const zIndex = options.zIndex ?? 70;
	const openUpThreshold = options.openUpThreshold ?? 180;
	const minHeight = options.minHeight ?? 120;

	const anchorRect = anchor.getBoundingClientRect();
	const viewportWidth = window.innerWidth || document.documentElement.clientWidth || 0;
	const viewportHeight = window.innerHeight || document.documentElement.clientHeight || 0;

	dropdown.style.position = "fixed";
	dropdown.style.zIndex = String(zIndex);
	dropdown.style.marginTop = "0";
	dropdown.style.minWidth = `${Math.max(minWidth, Math.round(anchorRect.width))}px`;
	dropdown.style.maxWidth = `${Math.max(minWidth, viewportWidth - viewportPadding * 2)}px`;
	dropdown.style.top = `${Math.round(anchorRect.bottom + 4)}px`;
	dropdown.style.left = `${Math.max(viewportPadding, Math.round(anchorRect.left))}px`;

	let dropdownRect = dropdown.getBoundingClientRect();
	const spaceBelow = viewportHeight - anchorRect.bottom - viewportPadding;
	const spaceAbove = anchorRect.top - viewportPadding;
	const shouldOpenUp = spaceBelow < openUpThreshold && spaceAbove > spaceBelow;
	const maxHeight = Math.max(minHeight, shouldOpenUp ? spaceAbove : spaceBelow);
	dropdown.style.maxHeight = `${Math.floor(maxHeight)}px`;

	if (shouldOpenUp) {
		const desiredTop = anchorRect.top - Math.min(dropdownRect.height, maxHeight) - 4;
		dropdown.style.top = `${Math.max(viewportPadding, Math.round(desiredTop))}px`;
	}

	dropdownRect = dropdown.getBoundingClientRect();
	const clampedLeft = Math.max(
		viewportPadding,
		Math.min(Math.round(anchorRect.left), Math.round(viewportWidth - dropdownRect.width - viewportPadding)),
	);
	dropdown.style.left = `${clampedLeft}px`;
}
