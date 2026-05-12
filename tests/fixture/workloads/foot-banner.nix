{pkgs, lib}: let
  fontConfig = pkgs.makeFontsConf {
    fontDirectories = [pkgs.dejavu_fonts];
  };
in
  pkgs.writeShellApplication {
    name = "foot-banner";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.foot
    ];
    text = ''
      set -euo pipefail

      export FONTCONFIG_FILE=${fontConfig}

      exec foot \
        --app-id=workload-foot-banner \
        --title=workload-foot-banner \
        --font='DejaVu Sans Mono:size=14' \
        --window-size-chars=120x32 \
        --fullscreen \
        -- bash --noprofile --norc -c '
          printf "\033[?25l\033[H\033[2J"
          printf "%s\n" \
            "================================================" \
            "  STEAMPIPE IN-TREE FIXTURE - FOOT BANNER       " \
            "  resolution 1280x720  renderer foot            " \
            "================================================" \
            "  This frame is deterministic. Any pixel-level  " \
            "  diff vs. the golden indicates a capture or    " \
            "  compositor regression.                        " \
            "================================================"
          exec sleep infinity
        '
    '';
  }
