{
  description = "choragos — deterministic plan-cycle orchestrator (MCP + CLI)";

  inputs = {
    nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pin the toolchain to the channel declared in rust-toolchain.toml so
        # there is a single version source of truth.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        # nixpkgs' buildRustPackage, but using the pinned oxalica toolchain instead of
        # nixpkgs' default rustc/cargo.
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
      in
      {
        packages.default = rustPlatform.buildRustPackage {
          pname = "choragos";
          version = "0.1.0";
          src = ./.;
          cargoLock.lockFile = ./Cargo.lock;
          nativeBuildInputs = [
            pkgs.pkg-config
            pkgs.makeWrapper
          ];
          buildInputs = [ ];

          # Both binaries shell out to git/gh at runtime; bake those onto PATH so the
          # wrapped binaries work outside a dev shell. bun/ai-coding are intentionally
          # NOT baked in here — they are provided by the caller's home-manager
          # environment via AI_CODING_MONOREPO.
          postInstall = ''
            for bin in choragos-mcp-server choragos; do
              wrapProgram $out/bin/$bin \
                --prefix PATH : ${pkgs.lib.makeBinPath [ pkgs.git pkgs.gh ]}
            done
          '';
        };

        devShells.default = pkgs.mkShell {
          nativeBuildInputs = [
            rustToolchain # Rust compiler, cargo, rustfmt, clippy
            pkgs.cargo-tarpaulin # >= 90% coverage enforcement (works on macOS)
            pkgs.nixpkgs-fmt # nix formatter, keeps `nix flake check` green
            pkgs.pkg-config
            pkgs.git
            pkgs.gh
          ];

          env = {
            RUST_BACKTRACE = "1";
          };
        };
      }
    );
}
