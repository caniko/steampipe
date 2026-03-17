{
  description = "NixOS microVM cluster orchestrator";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = {
    nixpkgs,
    rust-overlay,
    flake-utils,
    ...
  }:
    flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      toolchain = pkgs.rust-bin.nightly.latest.minimal;

      cluster-ctl = pkgs.rustPlatform.buildRustPackage {
        pname = "cluster-ctl";
        version = "0.1.0";
        src = ./.;
        cargoHash = "sha256-SCsLAaR1dMK0GNuvLIJ5PO23bQZhLg8XKA+cuwAdCRU=";
        nativeBuildInputs = [toolchain];
      };
    in {
      packages.default = cluster-ctl;
    });
}
