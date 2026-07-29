{
  cargoConfig,
  craneLib,
  cross,
  packages,
  pkgs,
  plinthProject,
  rsHarborLib,
  rustToolchain,
}: let
  # Shared libraries that iced/winit `dlopen` at runtime for the
  # `cluster-guard-prompt` Steam Guard dialog. On NixOS these are not on the
  # default loader path, so a bare `cargo run` fails with
  # `WaylandError(Connection(NoWaylandLib))`. Putting them on LD_LIBRARY_PATH in
  # the dev shell lets the dialog open under `cargo run`. (The installed package
  # gets the same set via makeWrapper — see nix/flake/packages.nix.)
  guiRuntimeLibs = [
    pkgs.wayland
    pkgs.libxkbcommon
    pkgs.libGL
    pkgs.vulkan-loader
    pkgs.fontconfig
    pkgs.freetype
    pkgs.libx11
    pkgs.libxcursor
    pkgs.libxi
    pkgs.libxrandr
    pkgs.libxcb
  ];
in
  (rsHarborLib.mkDevShells {
    inherit cargoConfig craneLib cross pkgs;

    packages =
      [
        rustToolchain
        pkgs.cargo-outdated
        pkgs.just
        pkgs.mdbook
        pkgs.pkg-config
        pkgs.wayland-protocols
        # Tools used by the desktop visual-rubric producer. The Steam Guard
        # prompt is an actual Iced/X11 window, so captures need a deterministic
        # virtual display and window-level screenshot rather than a browser.
        pkgs.xorg.xorgserver
        pkgs.xdotool
        pkgs.imagemagick
        packages.default
        (pkgs.python3.withPackages (ps: [ps.pillow]))
      ]
      ++ guiRuntimeLibs;

    extraShellHook = ''
      export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath guiRuntimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      echo "Documentation: cd docs && mdbook serve"
    '';
  })
  // {
    docs = rsHarborLib.mkDocsShell {
      inherit cargoConfig craneLib cross pkgs;
      packages =
        [
          rustToolchain
          pkgs.just
          pkgs.mdbook
          pkgs.pkg-config
          plinthProject
          pkgs.wayland-protocols
          packages.default
          (pkgs.python3.withPackages (ps: [ps.pillow]))
        ]
        ++ guiRuntimeLibs;
      extraShellHook = ''
        export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath guiRuntimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
        echo "Project site: plinth-project serve --config website/plinth-project.toml"
        echo "Documentation: mdbook serve docs"
      '';
    };
  }
