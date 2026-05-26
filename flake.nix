{
  description = "NixOS microVM cluster orchestrator";

  nixConfig = {
    extra-substituters = ["https://microvm.cachix.org"];
    extra-trusted-public-keys = ["microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
    microvm.url = "github:astro/microvm.nix";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    microvm,
    ...
  }:
    (flake-utils.lib.eachDefaultSystem (system: let
      root = ./.;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      inherit (pkgs) lib;

      toolchain = pkgs.rust-bin.nightly.latest.minimal.override {
        extensions = [
          "clippy"
          "rustfmt"
        ];
      };

      packages = import ./nix/flake/packages.nix {
        inherit lib pkgs root toolchain;
      };
    in {
      inherit packages;

      checks = import ./nix/flake/checks.nix {
        inherit lib microvm nixpkgs pkgs self system;
      };

      devShells = import ./nix/flake/dev-shells.nix {
        inherit packages pkgs toolchain;
      };
    }))
    // (import ./nix/flake/flake-modules.nix {
      inherit microvm nixpkgs self;
    })
    // {
      overlays.default = import ./nix/flake/overlay.nix {
        inherit microvm nixpkgs;
      };
    };
}
