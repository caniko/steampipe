{vmUser, workloads}: {
  config,
  lib,
  pkgs,
  ...
}: let
  runtimeDir = "/tmp/runtime-${vmUser}";
  workloadLibPath = config.steampipe.vmGraphics.runtimeLibraryPath;
  homeDir = "/home/${vmUser}";
  workloadHome = "${homeDir}/.workload";
  workloadStateDir = "${homeDir}/.local/state/workload";

  workloadCommon = ''
    export HOME=${homeDir}
    export XDG_RUNTIME_DIR=${runtimeDir}
    export WAYLAND_DISPLAY=wayland-1
    export LD_LIBRARY_PATH=${workloadLibPath}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
    mkdir -p "${workloadHome}" "${workloadStateDir}"
    chmod 700 "${runtimeDir}" 2>/dev/null || true

    if [ -z "''${SWAYSOCK:-}" ]; then
      socket="$(find "$XDG_RUNTIME_DIR" -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -n1 || true)"
      if [ -n "$socket" ]; then
        export SWAYSOCK="$socket"
      fi
    fi
  '';

  workloadControl = pkgs.writeShellApplication {
    name = "workload-control";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.findutils
      pkgs.gnugrep
      pkgs.procps
      pkgs.util-linux
    ];
    text = ''
      set -euo pipefail
      ${workloadCommon}

      pid_file="${workloadStateDir}/active.pid"
      name_file="${workloadStateDir}/active.name"
      link_path="${workloadHome}/current"

      wait_for_sway() {
        for _ in $(seq 1 30); do
          if swaymsg -r -t get_outputs >/dev/null 2>&1; then
            return 0
          fi
          sleep 0.1
        done
        echo "sway is not ready under $XDG_RUNTIME_DIR" >&2
        exit 1
      }

      stop_active() {
        if [ -f "$pid_file" ]; then
          pid="$(cat "$pid_file")"
          if kill -0 "$pid" 2>/dev/null; then
            kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
            for _ in $(seq 1 20); do
              if ! kill -0 "$pid" 2>/dev/null; then
                break
              fi
              sleep 0.1
            done
            if kill -0 "$pid" 2>/dev/null; then
              kill -KILL -- "-$pid" 2>/dev/null || kill -KILL "$pid" 2>/dev/null || true
            fi
          fi
        fi

        pkill -x foot 2>/dev/null || true
        pkill -x vkcube 2>/dev/null || true
        pkill -f '/bin/wgpu-checker' 2>/dev/null || true
        rm -f "$pid_file" "$name_file" "$link_path"
      }

      current_name() {
        if [ -f "$pid_file" ] && [ -f "$name_file" ]; then
          pid="$(cat "$pid_file")"
          if kill -0 "$pid" 2>/dev/null; then
            cat "$name_file"
            return 0
          fi
        fi
        echo none
      }

      tree_contains() {
        pattern="$1"
        swaymsg -r -t get_tree 2>/dev/null | grep -Eq "$pattern"
      }

      case "''${1:-}" in
        run)
          name="''${2:-}"
          if [ -z "$name" ]; then
            echo "usage: workload-run NAME" >&2
            exit 2
          fi
          case "$name" in
            foot-banner)
              target='${workloads.footBanner}/bin/foot-banner'
              pattern='"title"[[:space:]]*:[[:space:]]*"workload-foot-banner"'
              ;;
            vkcube-frozen)
              target='${workloads.vkcubeFrozen}/bin/vkcube-frozen'
              pattern='"app_id"[[:space:]]*:[[:space:]]*"vkcube"'
              ;;
            wgpu-checker)
              target='${workloads.wgpuChecker}/bin/wgpu-checker'
              pattern='"title"[[:space:]]*:[[:space:]]*"workload-wgpu-checker"'
              ;;
            *)
              echo "unknown workload: $name" >&2
              exit 2
              ;;
          esac

          wait_for_sway
          stop_active
          ln -sfn "$target" "$link_path"
          printf '%s\n' "$name" > "$name_file"
          setsid "$target" >"${workloadStateDir}/$name.log" 2>&1 &
          pid="$!"
          printf '%s\n' "$pid" > "$pid_file"

          for _ in $(seq 1 30); do
            if tree_contains "$pattern"; then
              exit 0
            fi
            sleep 0.1
          done

          echo "workload window did not appear: $name" >&2
          exit 1
          ;;
        stop)
          stop_active
          ;;
        current)
          current_name
          ;;
        *)
          echo "usage: workload-control {run NAME|stop|current}" >&2
          exit 2
          ;;
      esac
    '';
  };
in {
  environment.systemPackages = [
    workloads.footBanner
    workloads.vkcubeFrozen
    workloads.wgpuChecker
    pkgs.grim
    pkgs.pngcrush
    workloadControl
    (pkgs.writeShellApplication {
      name = "workload-run";
      runtimeInputs = [workloadControl];
      text = ''exec workload-control run "$@"'';
    })
    (pkgs.writeShellApplication {
      name = "workload-stop";
      runtimeInputs = [workloadControl];
      text = ''exec workload-control stop "$@"'';
    })
    (pkgs.writeShellApplication {
      name = "workload-current";
      runtimeInputs = [workloadControl];
      text = ''exec workload-control current "$@"'';
    })
  ];

  environment.etc."sway/fixture-workload.conf".text = ''
    output * mode 1280x720 bg #000000 solid_color
    default_border pixel 0
    focus_follows_mouse no
    mouse_warping none
    seat seat0 hide_cursor 5000
    for_window [title="workload-foot-banner"] border pixel 0, fullscreen enable
    for_window [app_id="vkcube"] border pixel 0, fullscreen enable
    for_window [title="workload-wgpu-checker"] border pixel 0, fullscreen enable
    exec_always --no-startup-id sh -lc 'test -x "$HOME/.workload/current" && "$HOME/.workload/current" >/dev/null 2>&1 &'
  '';

  systemd.tmpfiles.rules = [
    "d ${runtimeDir} 0700 ${vmUser} users -"
    "d ${workloadHome} 0755 ${vmUser} users -"
    "d ${workloadStateDir} 0755 ${vmUser} users -"
  ];

  systemd.services.fixture-sway = {
    description = "Autostart sway for fixture workloads";
    after = ["local-fs.target" "seatd.service"];
    wants = ["seatd.service"];
    wantedBy = ["multi-user.target"];
    serviceConfig = {
      User = vmUser;
      WorkingDirectory = homeDir;
      Restart = "always";
      RestartSec = 1;
      Environment = [
        "HOME=${homeDir}"
        "XDG_RUNTIME_DIR=${runtimeDir}"
        "WAYLAND_DISPLAY=wayland-1"
        "WLR_BACKENDS=drm"
        "WLR_RENDERER=gles2"
      ];
      ExecStart = "${pkgs.bash}/bin/bash -lc 'mkdir -p \"$XDG_RUNTIME_DIR\" && chmod 700 \"$XDG_RUNTIME_DIR\" && exec ${pkgs.sway}/bin/sway --config /etc/sway/fixture-workload.conf'";
    };
  };
}
