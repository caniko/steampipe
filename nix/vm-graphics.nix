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
#     libxkbcommon, libwayland, libGL, libvulkan, etc. without manual wrappers.
{
  config,
  lib,
  pkgs,
  ...
}: let
  graphicsEnabled = config.microvm.graphics.enable or false;
  cfg = config.steampipe.vmGraphics;

  # Runtime libs that wayland/wgpu/winit-style clients dlopen at startup.
  # Mesa/DRI is already on /run/opengl-driver/lib via hardware.graphics.enable.
  graphicalRuntimeLibs = with pkgs; [
    libxkbcommon
    wayland
    libGL
    vulkan-loader
    stdenv.cc.cc.lib
    libx11
    libxcursor
    libxext
    libxi
    libxrandr
    libxrender
    libxcb
  ];

  graphicalRuntimeLibPath = lib.makeLibraryPath graphicalRuntimeLibs;
in {
  options.steampipe.vmGraphics = {
    runtimeLibraryPath = lib.mkOption {
      type = lib.types.str;
      readOnly = true;
      default = graphicalRuntimeLibPath;
      description = ''
        Colon-separated library path for graphical clients deployed into the VM.
        Exported as STEAMPIPE_GRAPHICS_LIB_PATH (system-wide) and prepended to
        LD_LIBRARY_PATH for login shells when graphics are enabled.
      '';
    };

    softwareRendering = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = ''
        When true and microvm graphics are disabled, still publish the Wayland
        and Mesa runtime library path for software-rendered UIs. This mode does
        not export Vulkan-first environment like WGPU_BACKEND or VK_DRIVER_FILES.
      '';
    };
  };

  config = lib.mkMerge [
    (lib.mkIf graphicsEnabled {
      # Mesa/DRI so wgpu and other GPU clients can render on virtio-gpu.
      # The default UI-full path is Vulkan-first. Projects that need the older
      # GL/EGL virgl path should set WGPU_BACKEND=gl plus Mesa overrides in an
      # explicit experimental profile env table.
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
        export WGPU_BACKEND=vulkan
      '';

      environment.systemPackages = with pkgs; [
        grim
        mesa-demos
        vulkan-tools
        wayland-utils
        xdpyinfo
      ];
    })

    (lib.mkIf (!graphicsEnabled && cfg.softwareRendering) {
      # Mesa llvmpipe and Wayland client libraries for sway-headless,
      # wayvnc, CEF/GTK, and Steam login UI without virtio-gpu.
      hardware.graphics.enable = true;

      environment.sessionVariables = {
        WAYLAND_DISPLAY = "wayland-1";
      };

      environment.variables = {
        STEAMPIPE_GRAPHICS_LIB_PATH = graphicalRuntimeLibPath;
      };

      environment.etc."profile.d/steampipe-graphics.sh".text = ''
        export STEAMPIPE_GRAPHICS_LIB_PATH="${graphicalRuntimeLibPath}"
      '';

      environment.systemPackages = with pkgs; [
        mesa-demos
        wayland-utils
      ];
    })
  ];
}
