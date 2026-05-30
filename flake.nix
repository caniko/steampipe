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

    # Our patched steam-vent fork (the `vendor/steam-vent` git submodule). Nix
    # flakes don't pull submodule contents into the build source, so the same
    # branch is also taken as a non-flake input and spliced into the build tree
    # at `vendor/steam-vent` (see nix/flake/packages.nix). Keep this `ref` in
    # sync with the submodule commit when bumping the fork.
    steam-vent-fork = {
      url = "git+https://codeberg.org/caniko/steam-vent.git?ref=integration";
      flake = false;
    };
  };

  outputs = {
    self,
    nixpkgs,
    rs-harbor,
    rust-overlay,
    flake-utils,
    microvm,
    steam-vent-fork,
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
        inherit craneLib lib pkgs root steam-vent-fork;
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
