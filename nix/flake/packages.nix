{
  lib,
  pkgs,
  root,
  toolchain,
}: let
  docs = pkgs.stdenv.mkDerivation {
    pname = "steampipe-docs";
    version = "0.1.0";
    src = lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (root + /docs/book.toml)
        (root + /docs/src/SUMMARY.md)
        (root + /docs/src/introduction.md)
        (root + /docs/src/getting-started/installation.md)
        (root + /docs/src/getting-started/quick-start.md)
        (root + /docs/src/configuration/steampipe-toml.md)
        (root + /docs/src/configuration/steam-credentials.md)
        (root + /docs/src/configuration/vm-lifecycle.md)
        (root + /docs/src/testing/e2e-tests.md)
        (root + /docs/src/testing/history.md)
        (root + /docs/src/networking/cluster-network.md)
        (root + /docs/src/networking/network-simulation.md)
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
      inherit root;
      fileset = lib.fileset.maybeMissing (root + /website);
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
    src = root;
    cargoHash = "sha256-3QhzDItuizh7xP5qMgEi+0e2Qaad6C9M7zRHm6RfGio=";
    nativeBuildInputs = [toolchain pkgs.makeWrapper];
    postInstall = ''
      wrapProgram $out/bin/cluster-ctl \
        --prefix PATH : ${lib.makeBinPath [pkgs.weston]}
    '';
  };
in {
  default = cluster-ctl;
  inherit docs website site;
}
