{
  description = "Chelix - Personal AI gateway";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane.url = "github:ipetkov/crane";
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    crane,
  }:
    flake-utils.lib.eachDefaultSystem (
      system: let
        overlays = [(import rust-overlay)];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        nightly = "2025-11-30";
        wasmCraneLib =
          (crane.mkLib pkgs).overrideToolchain
          (
            p:
              p.rust-bin.nightly.${nightly}.default.override {
                targets = ["wasm32-wasip2"];
              }
          );

        # Pinned nightly to avoid recursion limit overflow in matrix-sdk
        # Latest nightly (2026-04) has query depth changes that break matrix-sdk 0.16
        rustToolchain = pkgs.rust-bin.nightly.${nightly}.default;

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Create a clean source that includes necessary files and the wit directory
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.cleanSourceFilter path type)
            || (builtins.match ".*/wit.*" path != null);
        };

        chelix-wasm-tools = wasmCraneLib.buildPackage {
          inherit src;
          pname = "chelix-wasm-tools";
          doCheck = false;
          cargoExtraArgs = "--target wasm32-wasip2 -p chelix-wasm-calc -p chelix-wasm-web-fetch -p chelix-wasm-web-search ";
          nativeBuildInputs = with pkgs;
            [
              rustPlatform.bindgenHook
              cmake
              perl
              pkg-config
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
            ];
        };
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = "chelix";
          version = "0.1.0";
          inherit src;
          doCheck = false;

          buildFeatures = [
            "embedded-assets"
            "embedded-wasm"
          ];
          preBuild = ''
            mkdir -p target/wasm32-wasip2/release/
            ln -s ${chelix-wasm-tools}/lib/* target/wasm32-wasip2/release/
            cargo build --release -p chelix-embedding-service
          '';
          cargoLock = {
            lockFile = ./Cargo.lock;
            outputHashes = {
              "sqlx-core-0.8.6" = "sha256-iZZlJ8YGlM1YUEGitK4aZH68tmg3y+gAVysXS8B+DW8=";
            };
          };
          nativeBuildInputs = with pkgs; [
            rustPlatform.bindgenHook
            cmake
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
