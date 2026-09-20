{
  description = "Chelix - Personal AI gateway";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        nightly = "2025-11-30";

        # Pinned nightly to avoid recursion limit overflow in matrix-sdk
        # Latest nightly (2026-04) has query depth changes that break matrix-sdk 0.16
        rustToolchain = pkgs.rust-bin.nightly.${nightly}.default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Create a clean source that includes the required project files.
        # Only the repository-root vendor/ is excluded: it is gitignored and
        # reconstructed in preBuild from the pinned fetch. Nested vendor
        # directories (e.g. crates/web/src/assets/*/vendor) are tracked assets.
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            pkgs.lib.cleanSourceFilter path type
            && !(type == "directory"
              && builtins.baseNameOf path == "vendor"
              && dirOf path == toString ./.);
        };

        mistralRev = "d5ae0f18f2170f10d30880cb7d21fb0880410e7b";
        mistralSrc = pkgs.fetchurl {
          url = "https://github.com/EricLBuehler/mistral.rs/archive/${mistralRev}.tar.gz";
          hash = "sha256-Lqw/mHs6YUrCK1tu7B74KhlZqO9r3WD08pL0qYaii0k=";
        };
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = "chelix";
          version = "0.1.0";
          inherit src;
          doCheck = false;

          buildFeatures = [
            "embedded-assets"
          ];
          preBuild = ''
            mkdir -p vendor
            tar -xzf ${mistralSrc} -C vendor
            mv vendor/mistral.rs-${mistralRev} vendor/mistral.rs
            for patch in patches/mistral.rs/*.patch; do
              patch -d vendor/mistral.rs -p1 < "$patch"
            done
            cargo build --release -p chelix-embedding-service
          '';
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
            outputHashes = {
              "sqlx-core-0.8.6" = "sha256-iZZlJ8YGlM1YUEGitK4aZH68tmg3y+gAVysXS8B+DW8=";
            };
          };
          nativeBuildInputs = with pkgs; [
            rustPlatform.bindgenHook
            cmake
            patch
            perl
            pkg-config
          ];
          cargoBuildFlags = ["--bin" "chelix"];
          postInstall = ''
            install -Dm755 target/release/chelix-embedding-service $out/bin/chelix-embedding-service
          '';
          CHELIX_VERSION = toString (self.shortRev or self.dirtyShortRev or self.lastModified or "nix");

          meta = with pkgs.lib; {
            description = "Personal AI gateway";
            homepage = "https://github.com/agentics-skills/chelix";
            license = licenses.mit;
            mainProgram = "chelix";
          };
        };

        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            rustPlatform.bindgenHook
            pkgs.rust-bin.nightly.${nightly}.default
            rust-analyzer
            cmake
            perl
            pkg-config
          ];
        };
      }
    );
}
