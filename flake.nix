{
  description = "NixOS microVM cluster orchestrator";

  nixConfig = {
    extra-substituters = ["https://microvm.cachix.org"];
    extra-trusted-public-keys = ["microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="];
  };

  inputs = {
    rs-harbor.url = "git+https://github.com/caniko/rs-harbor.git?ref=trunk&rev=f209ddbca3fdbb0dc31fa3886ccc2ff7369c18ac";
    nixpkgs.follows = "rs-harbor/nixpkgs";
    rust-overlay = {
      follows = "rs-harbor/rust-overlay";
    };
    crane.follows = "rs-harbor/crane";
    flake-utils.url = "github:numtide/flake-utils";
    microvm.url = "github:astro/microvm.nix";
    plinth = {
      url = "git+ssh://git@codeberg.org/caniko/plinth.git?ref=refs/heads/trunk";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rs-harbor.follows = "rs-harbor";
      inputs.nix-cache-pin.inputs.rs-harbor.follows = "rs-harbor";
      inputs.nix-pklx.url = "git+https://github.com/caniko/nix-pklx.git?ref=trunk&rev=541c3655e9251fdd047f96a4f30810fa21f89d2f";
      inputs.nix-pklx.inputs.rs-harbor.follows = "rs-harbor";
      inputs.nix-pklx.inputs.plinth.follows = "plinth";
      inputs.nix-pklx.inputs.plinth.inputs.rs-harbor.follows = "rs-harbor";
      inputs.nix-pklx.inputs.plinth.inputs.nix-cache-pin.inputs.rs-harbor.follows = "rs-harbor";
      inputs.nix-pklx.inputs.plinth.inputs.nix-pklx.inputs.rs-harbor.follows = "rs-harbor";
    };
  };

  outputs = {
    self,
    nixpkgs,
    rs-harbor,
    rust-overlay,
    flake-utils,
    microvm,
    plinth,
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

      toolchain = rs-harbor.lib.mkToolchain {inherit pkgs; toolchainProfile = "nightly";};
      inherit (toolchain) craneLib rustToolchain;

      cross = rs-harbor.lib.mkCross {inherit pkgs system;};
      cargoConfig = rs-harbor.lib.mkCargoConfig {
        inherit pkgs;
        channel = "nightly";
      };

      packages = import ./nix/flake/packages.nix {
        inherit craneLib lib pkgs plinth root system;
      };
    in {
      inherit packages;

      apps.deploy-pages = plinth.lib.${system}.mkDeployPagesApp {
        domain = "cluster-ctl.tartanoglu.com";
      };

      checks = import ./nix/flake/checks.nix {
        inherit lib microvm nixpkgs pkgs self system;
      };

      devShells = import ./nix/flake/dev-shells.nix {
        inherit cargoConfig craneLib cross packages pkgs rustToolchain;
        plinthProject = plinth.packages.${system}.plinth-project;
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
