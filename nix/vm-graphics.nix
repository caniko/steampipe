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
#   - Enables mesa/DRI drivers so wgpu/Vulkan can render on virtio-gpu
#   - Sets Wayland/X11 session variables for GUI applications
{
  config,
  lib,
  ...
}: let
  graphicsEnabled = config.microvm.graphics.enable or false;
in {
  config = lib.mkIf graphicsEnabled {
    # Mesa/DRI so wgpu and other GPU clients can render on virtio-gpu
    hardware.graphics.enable = true;

    # Default session variables for Wayland/X11 clients
    environment.sessionVariables = {
      WAYLAND_DISPLAY = "wayland-1";
      DISPLAY = ":0";
    };
  };
}
