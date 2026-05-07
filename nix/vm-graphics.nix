# NixOS module for VM-side virtio-gpu support.
#
# Usage in your microVM NixOS config:
#
#   modules = [
#     steampipe.nixosModules.microvm      # the microvm module itself
#     steampipe.nixosModules.vm-graphics  # this module
#     {
#       nixpkgs.overlays = [ steampipe.overlays.default ];
#       microvm.graphics.enable = true;
#       # ... rest of your VM config
#     }
#   ];
#
# The overlay must be applied separately (nixpkgs.overlays) because NixOS
# modules cannot inject overlays from a different evaluation scope.
#
# What this module does when microvm.graphics.enable is true:
#   - Enables mesa/DRI drivers so wgpu can render on virtio-gpu
#   - Sets Wayland session variables for GUI applications
#   - Exposes a graphical-app runtime library path so foreign-binary clients
#     (steampipe-deployed game binaries with no Nix rpath) can dlopen
#     libxkbcommon, libwayland, libGL, etc. without manual wrappers.
{
  config,
  lib,
  pkgs,
  ...
}: let
  graphicsEnabled = config.microvm.graphics.enable or false;

  # Runtime libs that wayland/wgpu/winit-style clients dlopen at startup.
  # Mesa/DRI is already on /run/opengl-driver/lib via hardware.graphics.enable.
  graphicalRuntimeLibs = with pkgs; [
    libxkbcommon
    wayland
    libGL
    stdenv.cc.cc.lib
  ];

  graphicalRuntimeLibPath = lib.makeLibraryPath graphicalRuntimeLibs;
in {
  options.steampipe.vmGraphics.runtimeLibraryPath = lib.mkOption {
    type = lib.types.str;
    readOnly = true;
    default = graphicalRuntimeLibPath;
    description = ''
      Colon-separated library path for graphical clients deployed into the VM.
      Exported as STEAMPIPE_GRAPHICS_LIB_PATH (system-wide) and prepended to
      LD_LIBRARY_PATH for login shells when graphics are enabled.
    '';
  };

  config = lib.mkIf graphicsEnabled {
    # Mesa/DRI so wgpu and other GPU clients can render on virtio-gpu.
    # The UI-full crosvm runner disables guest Vulkan, so the SSH-launched
    # game must use wgpu's GL backend and Mesa's virtio/virgl path. If wgpu can
    # see Vulkan here it may select a Mesa software adapter instead; if it
    # cannot see Vulkan and is not told to use GL it reports "Unable to find a
    # GPU".
    hardware.graphics.enable = true;

    # Default session variables for Wayland clients
    environment.sessionVariables = {
      WAYLAND_DISPLAY = "wayland-1";
    };

    # System-wide env: exposes the runtime lib path under a steampipe-namespaced
    # variable so consumers (e.g. SSH-launched binaries) can opt in explicitly.
    # We do NOT clobber LD_LIBRARY_PATH globally — that breaks system tooling
    # that links against newer libs on /run/current-system. Consumers prepend
    # this path themselves when launching their binary.
    environment.variables = {
      STEAMPIPE_GRAPHICS_LIB_PATH = graphicalRuntimeLibPath;
    };

    # /etc/profile.d/steampipe-graphics.sh — sourced by the steampipe SSH game
    # launcher (which is a non-login non-interactive shell that wouldn't pick
    # up environment.variables otherwise). Keep this file path stable:
    # `harness/runner.rs::launch_game` sources it explicitly.
    environment.etc."profile.d/steampipe-graphics.sh".text = ''
      export STEAMPIPE_GRAPHICS_LIB_PATH="${graphicalRuntimeLibPath}"
      export WGPU_BACKEND=gl
      export MESA_LOADER_DRIVER_OVERRIDE=virtio_gpu
      export GALLIUM_DRIVER=virgl
    '';
  };
}
