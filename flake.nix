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

      docs = pkgs.stdenv.mkDerivation {
        pname = "steampipe-docs";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./docs/book.toml
            ./docs/src/SUMMARY.md
            ./docs/src/introduction.md
            ./docs/src/getting-started/installation.md
            ./docs/src/getting-started/quick-start.md
            ./docs/src/configuration/steampipe-toml.md
            ./docs/src/configuration/steam-credentials.md
            ./docs/src/configuration/vm-lifecycle.md
            ./docs/src/testing/e2e-tests.md
            ./docs/src/testing/history.md
            ./docs/src/networking/cluster-network.md
            ./docs/src/networking/network-simulation.md
          ];
        };
        nativeBuildInputs = [pkgs.mdbook];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src docs
          cd docs/docs
          mdbook build
        '';
        installPhase = ''
          cp -r book $out
        '';
      };

      website = pkgs.stdenv.mkDerivation {
        pname = "steampipe-website";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.maybeMissing ./website;
        };
        nativeBuildInputs = [pkgs.zola];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src/website site
          cd site
          zola build
        '';
        installPhase = ''
          cp -r public $out
        '';
      };

      site = pkgs.runCommand "steampipe-site" {} ''
        mkdir -p $out
        cp -r ${website}/* $out/
        mkdir -p $out/docs
        cp -r ${docs}/* $out/docs/
      '';

      cluster-ctl = pkgs.rustPlatform.buildRustPackage {
        pname = "cluster-ctl";
        version = "0.1.0";
        src = ./.;
        cargoHash = "sha256-nCQmInUswiHYEXNPSU7eOU43xmw9jgbm+WlB7IM9cmI=";
        nativeBuildInputs = [toolchain pkgs.makeWrapper];
        postInstall = ''
          wrapProgram $out/bin/cluster-ctl \
            --prefix PATH : ${lib.makeBinPath [pkgs.weston pkgs.xwayland]} \
            --set-default STEAMPIPE_XWAYLAND_LD_LIBRARY_PATH ${
              lib.makeLibraryPath [
                pkgs.libx11
                pkgs.libxcursor
                pkgs.libXi
                pkgs.libXrandr
                pkgs.libglvnd
              ]
            }
        '';
      };
    in {
      packages = {
        default = cluster-ctl;
        inherit docs website site;
      };

      devShells.default = pkgs.mkShell {
        buildInputs = [
          toolchain
          pkgs.cargo-outdated
        ];
        packages = [
          pkgs.just
          pkgs.mdbook
          pkgs.zola
          cluster-ctl
        ];
        shellHook = ''
          echo "Website: cd website && zola serve"
          echo "Documentation: cd docs && mdbook serve"
        '';
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
