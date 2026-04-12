{
  description = "NixOS microVM cluster orchestrator";

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
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      toolchain = pkgs.rust-bin.nightly.latest.minimal;

      cluster-ctl = pkgs.rustPlatform.buildRustPackage {
        pname = "cluster-ctl";
        version = "0.1.0";
        src = ./.;
        cargoHash = "sha256-nCQmInUswiHYEXNPSU7eOU43xmw9jgbm+WlB7IM9cmI=";
        nativeBuildInputs = [toolchain];
      };
    in {
      packages.default = cluster-ctl;

      devShells.default = pkgs.mkShell {
        buildInputs = [
          toolchain
          pkgs.cargo-outdated
        ];
        packages = [
          pkgs.just
          cluster-ctl
        ];
      };
    }))
    // {
      nixosModules = microvm.nixosModules // {
        # Host networking (bridge, TAPs, NAT)
        default = import ./nix/module.nix;
        # VM-side graphics setup (mesa, virtio-gpu kernel modules, overlay)
        vm-graphics = import ./nix/vm-graphics.nix;
      };

      # Overlay providing cloud-hypervisor-graphics (from microvm.nix)
      overlays.default = microvm.overlay;
    };
}
