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
    adidoks = {
      url = "github:ES-Alexander/adidoks";
      flake = false;
    };
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    microvm,
    adidoks,
    ...
  }:
    (flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };

      toolchain = pkgs.rust-bin.nightly.latest.minimal;

      themeName =
        (builtins.fromTOML (builtins.readFile "${adidoks}/theme.toml")).name;

      docs = pkgs.stdenv.mkDerivation {
        pname = "steampipe-docs";
        version = "0.1.0";
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.maybeMissing ./docs/site;
        };
        nativeBuildInputs = [pkgs.zola];
        configurePhase = ''
          cd docs/site
          mkdir -p "themes/${themeName}"
          cp -r ${adidoks}/* "themes/${themeName}"
        '';
        buildPhase = ''
          zola build
        '';
        installPhase = ''
          cp -r public $out
        '';
      };

      website = pkgs.stdenv.mkDerivation {
        pname = "steampipe-website";
        version = "0.1.0";
        src = pkgs.lib.fileset.toSource {
          root = ./.;
          fileset = pkgs.lib.fileset.maybeMissing ./website;
        };
        nativeBuildInputs = [pkgs.zola];
        buildPhase = ''
          cd website
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
        nativeBuildInputs = [toolchain];
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
          pkgs.zola
          cluster-ctl
        ];
        shellHook = ''
          # Set up adidoks theme symlink for local docs development
          if [ -d docs/site ]; then
            mkdir -p docs/site/themes
            ln -sfn "${adidoks}" "docs/site/themes/${themeName}"
          fi
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
