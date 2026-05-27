{
  description = "NixOS microVM cluster orchestrator";

  nixConfig = {
    extra-substituters = ["https://microvm.cachix.org"];
    extra-trusted-public-keys = ["microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="];
  };

  inputs = {
    rs-harbor.url = "git+https://codeberg.org/caniko/rs-harbor.git";
    nixpkgs.follows = "rs-harbor/nixpkgs";
    rust-overlay = {
      follows = "rs-harbor/rust-overlay";
    };
    crane.follows = "rs-harbor/crane";
    flake-utils.follows = "rs-harbor/flake-utils";
    microvm.url = "github:astro/microvm.nix";
  };

  outputs = {
    self,
    nixpkgs,
    rs-harbor,
    rust-overlay,
    flake-utils,
    microvm,
    ...
  }:
    # Steampipe is x86_64-linux only: the cluster targets QEMU/KVM,
    # cloud-hypervisor, and crosvm — all of which we run exclusively on
    # x86_64 hardware. Restricting the system set here also keeps
    # `nix flake check --all-systems` from doubling the evaluator's
    # peak heap by re-evaluating every NixOS check on aarch64.
    (flake-utils.lib.eachSystem ["x86_64-linux"] (system: let
      root = ./.;
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      inherit (pkgs) lib;

      toolchain = rs-harbor.lib.mkToolchain {inherit pkgs;};
      inherit (toolchain) craneLib rustToolchain;

      cross = rs-harbor.lib.mkCross {inherit pkgs system;};
      cargoConfig = rs-harbor.lib.mkCargoConfig {
        inherit pkgs;
        channel = "nightly";
      };

      packages = import ./nix/flake/packages.nix {
        inherit craneLib lib pkgs root;
      };
    in {
      inherit packages;

      checks = import ./nix/flake/checks.nix {
        inherit lib microvm nixpkgs pkgs self system;
      };

      devShells = import ./nix/flake/dev-shells.nix {
        inherit cargoConfig craneLib cross packages pkgs rustToolchain;
        rsHarborLib = rs-harbor.lib;
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
