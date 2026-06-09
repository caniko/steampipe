{
  craneLib,
  lib,
  pkgs,
  plinth,
  root,
  system,
}: let
  docs = pkgs.stdenv.mkDerivation {
    pname = "steampipe-docs";
    version = "0.1.0";
    src = lib.fileset.toSource {
      inherit root;
      fileset = lib.fileset.unions [
        (root + /docs/book.toml)
        (root + /docs/src)
      ];
    };
    nativeBuildInputs = [pkgs.mdbook];
    phases = ["buildPhase" "installPhase"];
    buildPhase = ''
      cp -r --no-preserve=mode $src/docs docs
      mdbook build docs
    '';
    installPhase = ''
      cp -r docs/book $out
    '';
  };

  website = plinth.lib.${system}.mkProjectSite {
    pname = "cluster-ctl-website";
    domain = "cluster-ctl.tartanoglu.com";
    configPath = root + /website/plinth-project.toml;
    docsPackage = docs;
  };

  site = website;

  # steam-vent is a cargo git dependency (our patched fork's `integration`
  # branch); crane vendors it from Cargo.lock, so the build source is just
  # steampipe's own tree.
  src = craneLib.cleanCargoSource root;

  commonArgs = {
    inherit src;
    strictDeps = true;
  };

  clusterCtlArgs = commonArgs // {
    cargoExtraArgs = "--package steampipe --bin cluster-ctl";
    nativeBuildInputs = [pkgs.makeWrapper];
  };

  clusterCtlCargoArtifacts = craneLib.buildDepsOnly clusterCtlArgs;

  cluster-ctl = craneLib.buildPackage (clusterCtlArgs
    // {
      pname = "cluster-ctl";
      version = "0.1.0";
      cargoArtifacts = clusterCtlCargoArtifacts;
      postInstall = ''
        wrapProgram $out/bin/cluster-ctl \
          --prefix PATH : ${lib.makeBinPath [pkgs.weston cluster-guard-prompt]}
      '';
    });

  guiRuntimeLibs = [
    pkgs.fontconfig
    pkgs.freetype
    pkgs.libGL
    pkgs.libx11
    pkgs.libxcb
    pkgs.libxcursor
    pkgs.libxi
    pkgs.libxkbcommon
    pkgs.libxrandr
    pkgs.vulkan-loader
    pkgs.wayland
  ];

  guardPromptArgs = commonArgs // {
    cargoExtraArgs = "--package cluster-guard-prompt --bin cluster-guard-prompt";
    nativeBuildInputs = [
      pkgs.makeWrapper
      pkgs.pkg-config
    ];
    buildInputs =
      guiRuntimeLibs
      ++ [
        pkgs.wayland-protocols
      ];
  };

  guardPromptCargoArtifacts = craneLib.buildDepsOnly guardPromptArgs;

  cluster-guard-prompt = craneLib.buildPackage (guardPromptArgs
    // {
      pname = "cluster-guard-prompt";
      version = "0.1.0";
      cargoArtifacts = guardPromptCargoArtifacts;
      postInstall = ''
        wrapProgram $out/bin/cluster-guard-prompt \
          --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath guiRuntimeLibs} \
          --suffix LD_LIBRARY_PATH : /run/opengl-driver/lib \
          --set-default LIBGL_DRIVERS_PATH /run/opengl-driver/lib/dri \
          --set-default __EGL_VENDOR_LIBRARY_DIRS /run/opengl-driver/share/glvnd/egl_vendor.d \
          --set-default XDG_DATA_DIRS ${pkgs.gsettings-desktop-schemas}/share/gsettings-schemas/${pkgs.gsettings-desktop-schemas.name}:${pkgs.gtk3}/share/gsettings-schemas/${pkgs.gtk3.name}:/run/opengl-driver/share
      '';
    });
in {
  default = cluster-ctl;
  inherit cluster-ctl cluster-guard-prompt docs site website;
}
