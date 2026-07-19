// ── Images/Sandboxes page English strings ────────────────

export default {
	// ── Page-level ──────────────────────────────────────────
	title: "Sandboxes",
	description:
		"Container images cached by chelix for sandbox execution. You can delete individual images or prune all. Build custom images from a base with apt packages.",
	appleContainerNote:
		"Apple Container provides VM-isolated execution but does not support building images. Docker (or OrbStack) is required alongside Apple Container to build and cache custom images. Sandboxed commands run via Apple Container; image builds use Docker.",
	sandboxDisabledHint:
		'Sandbox mode is Off. Commands execute directly on the host. Set sandbox.mode = "On" and restart Chelix to use isolated execution.',
	noCachedImages: "No cached images.",

	// ── Prune ──────────────────────────────────────────────
	pruneAll: "Prune all",
	pruning: "Pruning\u2026",

	// ── Default image selector ─────────────────────────────
	defaultImage: {
		title: "Default image",
		description:
			"Global base image used for sandbox execution. Leave empty to use the built-in default (ubuntu:26.04).",
	},

	// ── Image row ──────────────────────────────────────────
	deleteImage: "Delete image",

	// ── Build section ──────────────────────────────────────
	build: {
		title: "Build custom image",
		imageNameLabel: "Image name",
		baseImageLabel: "Base image",
		packagesLabel: "Packages (space or newline separated)",
		buildButton: "Build",
		building: "Building\u2026",
		buildingImage: "Building image\u2026",
		checkingPackages: "Checking packages in base image\u2026",
		noPackages: "Please specify at least one package.",
		builtTag: "Built: {{tag}}",
		errorPrefix: "Error: {{message}}",
		allPresent: "All requested packages are already present in {{base}}: {{packages}}. No image build needed.",
		alreadyInBase: "Already in {{base}}: {{present}}. Only installing: {{missing}}.",
	},

	// ── Backend labels ─────────────────────────────────────
	backend: {
		appleContainer: "Apple Container (VM-isolated)",
		docker: "Docker",
		wasm: "Wasmtime (WASM-isolated)",
		none: "Off (direct host execution)",
		containerBackendLabel: "Container backend:",
	},

	// ── Recommendations ────────────────────────────────────
	recommendation: {
		macosDockerTip:
			"Apple Container provides stronger VM-level isolation on macOS 26+. Install it for automatic use (chelix prefers it over Docker). Run: brew install container",
		linuxDockerTip: "Docker provides filesystem-isolated execution. Podman and the WASM backend are also supported.",
		wasmTip:
			"Using WASM sandbox with filesystem isolation. For container-level isolation, install Docker or Apple Container.",
	},

	// ── Alert labels ───────────────────────────────────────
	alertWarning: "Warning: ",
	alertTip: "Tip: ",
};
