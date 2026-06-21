{
  description = "Agentline — bridge any IM to any coding agent";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, rust-overlay, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        rustToolchain = pkgs.rust-bin.stable."1.89.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" ];
          targets = [
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
            "x86_64-unknown-linux-gnu"
            "x86_64-pc-windows-gnu"
          ];
        };

        darwinDeps = pkgs.lib.optionals pkgs.stdenv.isDarwin (with pkgs.darwin.apple_sdk.frameworks; [
          SystemConfiguration
          Security
          CoreFoundation
          AppKit
          WebKit
        ]);
      in
      {
        devShells.default = pkgs.mkShell {
          buildInputs = [
            rustToolchain
            pkgs.nodejs_20
            pkgs.pkg-config
            pkgs.librsvg
          ] ++ darwinDeps;

          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
        };
      }
    );
}
