{
  description = "NixOS microVM cluster orchestrator";

  nixConfig = {
    extra-substituters = ["https://microvm.cachix.org"];
    extra-trusted-public-keys = ["microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="];
  };

  inputs = {
    harbor-rs.url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=7a3328e186258dca31f9801227bc4e6fd8db4f36";
    nixpkgs.follows = "harbor-rs/nixpkgs";
    rust-overlay = {
      follows = "harbor-rs/rust-overlay";
    };
    crane.follows = "harbor-rs/crane";
    flake-utils.url = "github:numtide/flake-utils";
    microvm.url = "github:astro/microvm.nix";
    plinth = {
      url = "git+https://github.com/caniko/plinth.git?ref=refs/heads/trunk";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.nix-pklx.url = "git+https://github.com/caniko/nix-pklx.git?ref=trunk&rev=4e5dbefa4c94bb8af3c9e400a1a57790a39b518b";
      inputs.nix-pklx.inputs.plinth.follows = "plinth";
      inputs.nix-pklx.inputs.plinth.inputs.harbor-rs.follows = "harbor-rs";
      inputs.nix-pklx.inputs.plinth.inputs.nix-cache-pin.inputs.harbor-rs.follows = "harbor-rs";
      inputs.nix-pklx.inputs.plinth.inputs.nix-pklx.inputs.harbor-rs.follows = "harbor-rs";
    };
  };

  outputs = {
    self,
    nixpkgs,
    harbor-rs,
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

      toolchain = harbor-rs.lib.mkToolchain {inherit pkgs; toolchainProfile = "nightly";};
      inherit (toolchain) craneLib rustToolchain;

      cross = harbor-rs.lib.mkCross {inherit pkgs system;};
      cargoConfig = harbor-rs.lib.mkCargoConfig {
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
        rsHarborLib = harbor-rs.lib;
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
