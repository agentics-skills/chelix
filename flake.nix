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
        nightly = "2026-07-30";

        rustToolchain = pkgs.rust-bin.nightly.${nightly}.default.override {
          targets = ["wasm32-wasip2"];
        };

        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };

        # Create a clean source that includes the required project files
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = pkgs.lib.cleanSourceFilter;
        };
      in {
        packages.default = rustPlatform.buildRustPackage {
          pname = "chelix";
          version = "0.1.0";
          inherit src;
          doCheck = false;

          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };
          nativeBuildInputs = with pkgs; [
            cmake
            jq
            perl
            pkg-config
          ];
          postInstall = ''
            bash scripts/build-plugins.sh --release
            mkdir -p "$out/bin/plugins"
            cp -R target/release/plugins/. "$out/bin/plugins/"
          '';
          cargoBuildFlags = ["--bin" "chelix"];
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
