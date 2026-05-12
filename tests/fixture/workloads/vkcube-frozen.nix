{pkgs}:
pkgs.writeShellApplication {
  name = "vkcube-frozen";
  runtimeInputs = [
    pkgs.coreutils
    pkgs.vulkan-tools
  ];
  text = ''
    set -euo pipefail

    # vkcube destroys its Wayland surface when it exits after `--c 1`, so keep
    # relaunching the same first frame to leave a stable cube on screen.
    while true; do
      vkcube --wsi wayland --width 1280 --height 720 --present_mode 2 --c 1
      sleep 0.05
    done
  '';
}
