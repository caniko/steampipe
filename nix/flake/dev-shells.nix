{
  cargoConfig,
  craneLib,
  cross,
  packages,
  pkgs,
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
  rsHarborLib.mkDevShells {
    inherit cargoConfig craneLib cross pkgs;

    packages =
      [
        rustToolchain
        pkgs.cargo-outdated
        pkgs.just
        pkgs.mdbook
        pkgs.zola
        pkgs.pkg-config
        pkgs.wayland-protocols
        packages.default
        (pkgs.python3.withPackages (ps: [ps.pillow]))
      ]
      ++ guiRuntimeLibs;

    extraShellHook = ''
      export LD_LIBRARY_PATH="${pkgs.lib.makeLibraryPath guiRuntimeLibs}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
      echo "Website: cd website && zola serve"
      echo "Documentation: cd docs && mdbook serve"
    '';
  }
