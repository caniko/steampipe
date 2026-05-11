# Test cluster: NixOS microVMs for multiplayer testing
#
# Hypervisor and graphics are configured in steampipe.toml [cluster]:
#   hypervisor = "qemu"   - qemu, cloud-hypervisor, firecracker, crosvm, etc.
#   graphics   = true     - enable virtio-gpu (mesa/DRI in guest)
#
# Two first-class modes:
#   1v1        - 1 VM  + host = 2 players    (nix run .#cluster-1v1-*)
#   tournament - 7 VMs + host = 8 players    (nix run .#cluster-tournament-*)
#
# Network: handled by steampipe NixOS module (services.steampipe-cluster)
#   Bridge + TAPs + NAT created at boot. Fallback: sudo cluster-ctl net-up
{
  pkgs,
  lib,
  nixpkgs,
  clusterCtl,
  steampipe,
  projectRoot,
  projectConfig ? builtins.fromTOML (builtins.readFile (projectRoot + "/steampipe.toml")),
  extraVmOverlays ? [],
  # Enable NixOS-level integration: reads config from /etc/steampipe/module.json
  # written by the steampipe NixOS module. Provides loginStateDir and credentials
  # path automatically. Must be turned on explicitly.
  nixosIntegration ? false,
  # Project-level: credentials file for direct auto-login on boot.
  # Only used when nixosIntegration is false.
  credentials ? null,
  systemdStage1Default ? false,
}: let
  moduleConfigPath = "/etc/steampipe/module.json";
  moduleConfig =
    if nixosIntegration
    then builtins.fromJSON (builtins.readFile moduleConfigPath)
    else null;

  loginStateDir =
    if nixosIntegration
    then moduleConfig.loginStateDir
    else null;
  nixosCredentials =
    if nixosIntegration
    then moduleConfig.credentialsPath
    else null;
in
  assert lib.assertMsg (!(nixosIntegration && credentials != null))
  "credentials cannot be set when nixosIntegration is enabled - the NixOS module provides credentials"; let
    clusterConfig = projectConfig.cluster or {};
    vmHypervisor = clusterConfig.hypervisor or "cloud-hypervisor";
    vmGraphics = clusterConfig.graphics or true;
    vmUser = projectConfig.vm_user or "cluster";
    sshKey = projectConfig.ssh_key or "nix/test-cluster/cluster_key";
    sshPublicKey = let
      publicKeyPath =
        if lib.hasSuffix ".pub" sshKey
        then projectRoot + "/${sshKey}"
        else projectRoot + "/${sshKey}.pub";
    in
      assert lib.assertMsg (builtins.pathExists publicKeyPath)
      "steampipe.lib.mkTestCluster requires an SSH public key at ${toString publicKeyPath}; set ssh_key in steampipe.toml or create the .pub file next to the private cluster key";
        lib.removeSuffix "\n" (builtins.readFile publicKeyPath);

    # Cluster flavors: the default flavor matches `[cluster]`, additional
    # flavors are read from `[cluster.flavors.<name>]` in steampipe.toml. Each
    # flavor builds its own microvm runner-dir, gets its own `--cluster <mode>-<flavor>`
    # lease key, and produces a parallel script set
    # `cluster-<mode>-<flavor>-{up,down,test,...}` so flavors run side-by-side
    # without colliding on VM index, state dir, lease, or test history.
    extraFlavors = clusterConfig.flavors or {};
    mkFlavor = f: let
      hypervisor = f.hypervisor or vmHypervisor;
    in {
      inherit hypervisor;
      graphics = f.graphics or vmGraphics;
      memory = f.memory or null;
      gpuProfile = f.gpu_profile or "vulkan";
      useSystemdStage1 =
        if f ? use_systemd_stage1
        then f.use_systemd_stage1
        else if hypervisor == "crosvm"
        then systemdStage1Default
        else null;
    };
    flavors =
      {default = mkFlavor {};}
      // lib.mapAttrs (_n: mkFlavor) extraFlavors;
    # Cluster network configuration
    network = {
      bridge = "br-cluster";
      subnet = "10.0.100";
      prefix = 24;
      hostIp = "10.0.100.254";
    };

    vmCount = 7;

    # VM definitions (host machine is one extra player)
    linuxVMs =
      lib.genAttrs
      (map (i: "vm-${toString i}") (lib.range 1 vmCount))
      (name: let
        index = lib.toInt (lib.removePrefix "vm-" name);
      in {
        ip = "${network.subnet}.${toString index}";
        mac = "52:54:00:cb:00:0${toString index}";
        memory = 2048;
        cores = 1;
        inherit index;
      });

    # -- microVM NixOS configurations ------------------------------------

    # mkMicroVM builds a microVM runner.
    # loginMount controls how Steam login state is handled:
    #   null        - no login state sharing (default)
    #   "read"      - mount loginStateDir/vm-N read-only at /mnt/steam-login,
    #                  systemd copies files into writable home on boot
    #   "write"     - mount loginStateDir/vm-N as the VM user's home (read-write),
    #                  used by login runners so Steam writes directly to host
    mkMicroVM = flavor: loginMount: name: vm:
      (nixpkgs.lib.nixosSystem {
        system = "x86_64-linux";
        modules = [
          steampipe.nixosModules.microvm
          steampipe.nixosModules.vm-graphics
          steampipe.nixosModules.cluster-vm-base
          ({pkgs, ...}: {
            nixpkgs.overlays =
              [steampipe.overlays.default]
              ++ extraVmOverlays;

            steampipe.clusterVm = {
              user = vmUser;
              sshAuthorizedKeys = [sshPublicKey];
              enableSteam = true;
              useSystemdStage1 = flavor.useSystemdStage1;
            };

            networking.hostName = name;
            environment.sessionVariables = lib.mkIf (flavor.gpuProfile != "egl") {
              VK_DRIVER_FILES = "/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json";
            };
            networking.interfaces.eth0.ipv4.addresses = [
              {
                address = vm.ip;
                prefixLength = network.prefix;
              }
            ];

            microvm = {
              hypervisor = flavor.hypervisor;
              graphics.enable = flavor.graphics;
              # crosvm's multi-process device proxy uses minijail+seccomp.
              # We point crosvm at the seccomp policies installed by our
              # patched crosvm package; the pre-compiled `.bpf` form is what
              # actually gets loaded (text-mode parsing chokes on intentional
              # syscall redefinitions across the `@include` chain).
              #
              # `--no-usb` skips the xhci proxy fork. The xhci proxy is the
              # first device crosvm tries to create, and on this host its
              # post-fork `EventLoop::start` hits a thread spawn failure that
              # the parent observes as `Failed to configure tube: Connection
              # reset by peer`. The guest doesn't need USB for our test
              # workload, so disabling the device sidesteps the issue.
              crosvm.extraArgs = lib.mkIf (flavor.hypervisor == "crosvm") [
                "--seccomp-policy-dir=${pkgs.crosvm}/share/policy/${pkgs.stdenv.hostPlatform.linuxArch}"
                "--no-usb"
              ];
              mem =
                if flavor.memory != null
                then flavor.memory
                else vm.memory;
              vcpu = vm.cores;
              vsock.cid = vm.index + 3; # CIDs 0-2 are reserved

              # TAP interface (pre-created by NixOS module, attached to br-cluster)
              interfaces = [
                {
                  type = "tap";
                  id = "tap-${name}";
                  mac = vm.mac;
                }
              ];

              # Home volume: disk image unless login-write mode replaces it with virtiofs
              volumes = lib.optionals (loginMount != "write") [
                {
                  mountPoint = "/home/${vmUser}";
                  image = "${name}-home.img";
                  size = 8192;
                }
              ];

              # virtiofs shares for login state
              shares = lib.optionals (loginMount != null && loginStateDir != null) [
                (
                  if loginMount == "write"
                  then {
                    # Login runners: writable mount as home - Steam writes directly to host
                    tag = "steam-login-${name}";
                    source = "${loginStateDir}/${name}";
                    mountPoint = "/home/${vmUser}";
                    proto = "virtiofs";
                  }
                  else {
                    # Regular runners: read-only staging mount
                    tag = "steam-login-${name}";
                    source = "${loginStateDir}/${name}";
                    mountPoint = "/mnt/steam-login";
                    proto = "virtiofs";
                    readOnly = true;
                  }
                )
              ];
            };
          })

          # Copy login state from read-only share into writable home on boot
          (lib.mkIf (loginMount == "read") {
            systemd.services.steam-login-inject = {
              description = "Inject Steam login state from system-level share";
              after = ["local-fs.target"];
              wantedBy = ["multi-user.target"];
              serviceConfig = {
                Type = "oneshot";
                RemainAfterExit = true;
                User = vmUser;
              };
              script = ''
                src="/mnt/steam-login/.local/share/Steam"
                dst="/home/${vmUser}/.local/share/Steam"
                if [ -d "$src/config" ] && [ -f "$src/config/loginusers.vdf" ]; then
                  mkdir -p "$dst/config"
                  cp -a "$src/config"/* "$dst/config"/
                  cp -a "$src"/ssfn* "$dst"/ 2>/dev/null || true
                  if [ -f "$src/registry.vdf" ]; then
                    cp -a "$src/registry.vdf" "$dst"/
                  fi
                  echo "Steam login state injected from system share"
                else
                  echo "No system-level Steam login state found at $src"
                fi
              '';
            };
          })
        ];
      })
    .config
    .microvm
    .declaredRunner;

    # Build all VM runners, per flavor.
    # When loginStateDir is set, regular VMs read login state from the share.
    vmLoginMount =
      if loginStateDir != null
      then "read"
      else null;

    # The normal UI-full runner is Vulkan-first and switches microvm.nix's
    # crosvm virtio-gpu defaults to Mesa Venus over virglrenderer. The separate
    # uifull-egl flavor keeps the old GL/EGL experiment path by narrowing the
    # capsets to virgl/virgl2/drm and disabling guest Vulkan.
    patchCrosvmGpuParams = flavor: runner:
      if flavor.gpuProfile == "egl"
      then
        pkgs.runCommand "${runner.name}-egl-virgl-scanout" {} ''
          mkdir -p "$out"
          cp -aL ${runner}/. "$out"/
          chmod -R u+w "$out"
          substituteInPlace "$out/bin/microvm-run" \
            --replace-fail '"context-types":"virgl:virgl2:cross-domain"' '"backend":"virglrenderer","context-types":"virgl:virgl2:drm"' \
            --replace-fail '"vulkan":true' '"vulkan":false'
        ''
      else
        pkgs.runCommand "${runner.name}-venus-vulkan" {} ''
          mkdir -p "$out"
          cp -aL ${runner}/. "$out"/
          chmod -R u+w "$out"
          substituteInPlace "$out/bin/microvm-run" \
            --replace-fail '"context-types":"virgl:virgl2:cross-domain"' '"backend":"virglrenderer","context-types":"virgl:virgl2:venus:cross-domain","external-blob":true,"system-blob":true,"udmabuf":true,"implicit-render-server":true'
        '';

    takeFirstVMs = count:
      lib.filterAttrs (_name: vm: vm.index <= count) linuxVMs;

    mkRunnersDir = flavorName: flavor: count:
      pkgs.linkFarm "cluster-vm-runners-${flavorName}"
      (lib.mapAttrsToList (name: vm: {
          inherit name;
          path =
            if flavor.hypervisor == "crosvm"
            then patchCrosvmGpuParams flavor (mkMicroVM flavor vmLoginMount name vm)
            else mkMicroVM flavor vmLoginMount name vm;
        })
        (takeFirstVMs count));

    # Full default runner set for utilities that operate on the whole cluster.
    vmRunnersDir = mkRunnersDir "default" flavors.default vmCount;

    # Higher-memory runners for Steam login wizard (Sway + VNC + Steam GUI).
    # Login runners always use the default flavor - they don't need flavor variants.
    loginVMs = lib.mapAttrs (_name: vm: vm // {memory = 4096;}) linuxVMs;
    # Login runners: if loginStateDir is set, mount it writable so Steam
    # writes login state directly to host. Otherwise use disk image.
    loginLoginMount =
      if loginStateDir != null
      then "write"
      else null;
    loginVMRunners = lib.mapAttrs (mkMicroVM flavors.default loginLoginMount) loginVMs;

    loginVMRunnersDir =
      pkgs.linkFarm "cluster-login-vm-runners"
      (lib.mapAttrsToList (name: runner: {
          inherit name;
          path = runner;
        })
        loginVMRunners);

    # -- Wrapper helpers --------------------------------------------------

    ctl = "${clusterCtl}/bin/cluster-ctl";
    credsFlag = lib.optionalString (credentials != null) "--credentials ${credentials}";
    systemCredentialsPath = "/etc/steampipe/credentials.toml";
    hostOpenGLDriverPath = "/run/opengl-driver";
    hostRuntimeLibraryPath = pkgs.lib.makeLibraryPath [
      pkgs.vulkan-loader
      pkgs.libxkbcommon
      pkgs.stdenv.cc.cc
    ];
    hostPkgConfigPath = pkgs.lib.makeSearchPathOutput "dev" "lib/pkgconfig" [
      pkgs.wayland
      pkgs.libxkbcommon
      pkgs.udev
      pkgs.alsa-lib
    ];
    hostClusterEnv = ''
      if [ -z "''${XDG_RUNTIME_DIR:-}" ]; then
        export XDG_RUNTIME_DIR="/run/user/$(id -u)"
      fi
      if [ -z "''${WAYLAND_DISPLAY:-}" ]; then
        if [ -S "$XDG_RUNTIME_DIR/wayland-1" ]; then
          export WAYLAND_DISPLAY=wayland-1
        elif [ -S "$XDG_RUNTIME_DIR/wayland-0" ]; then
          export WAYLAND_DISPLAY=wayland-0
        fi
      fi
      export LD_LIBRARY_PATH=${hostOpenGLDriverPath}/lib:${hostRuntimeLibraryPath}''${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
      export LIBGL_DRIVERS_PATH=${hostOpenGLDriverPath}/lib/dri''${LIBGL_DRIVERS_PATH:+:$LIBGL_DRIVERS_PATH}
      export __EGL_VENDOR_LIBRARY_DIRS=${hostOpenGLDriverPath}/share/glvnd/egl_vendor.d''${__EGL_VENDOR_LIBRARY_DIRS:+:$__EGL_VENDOR_LIBRARY_DIRS}
      if [ -d ${hostOpenGLDriverPath}/lib/gbm ]; then
        export GBM_BACKENDS_PATH=${hostOpenGLDriverPath}/lib/gbm''${GBM_BACKENDS_PATH:+:$GBM_BACKENDS_PATH}
      fi
      export PKG_CONFIG_PATH=${hostPkgConfigPath}''${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}
    '';

    mkWrapper = name: count: args:
      pkgs.writeShellScript "cluster-${name}" ''
        ${hostClusterEnv}
        exec ${ctl} --vm-count ${toString count} ${credsFlag} ${args} "$@"
      '';

    # `clusterName` is what appears after `--cluster`; doubles as the lease key
    # and state-dir partition. For the default flavor it's just `<mode>`; for
    # other flavors it's `<mode>-<flavor>` so leases and state stay isolated.
    mkLanTestWrapper = scriptName: clusterName: count: players:
      pkgs.writeShellScript "cluster-${scriptName}-test" ''
        ${hostClusterEnv}
        disable_steam_env=1
        case " $* " in
          *" --network steam "*|*" --network=steam "*) disable_steam_env=0 ;;
        esac
        if [ "$disable_steam_env" = 1 ]; then
          export CHESSBENDER_DISABLE_STEAM=1
        fi
        exec ${pkgs.nix}/bin/nix develop -c \
          ${ctl} --vm-count ${toString count} ${credsFlag} --cluster ${clusterName} test --players ${toString players} "$@"
      '';

    mkSteamLoginWrapper = name: count: args:
      pkgs.writeShellScript "cluster-${name}" ''
        ${hostClusterEnv}
        credentials_args=()
        ${lib.optionalString (credentials == null) ''
          if [ -f ${systemCredentialsPath} ]; then
            credentials_args=(--credentials ${systemCredentialsPath})
          fi
        ''}
        exec ${ctl} --vm-count ${toString count} ${credsFlag} "''${credentials_args[@]}" ${args} "$@"
      '';

    # Per-mode, per-flavor scripts: lifecycle, deploy, test, run.
    #
    # Usage:
    #   mkModeScripts "1v1"       "default" 1 2    -> cluster-1v1-{up,test,...}
    #   mkModeScripts "1v1"       "uifull"  1 2    -> cluster-1v1-uifull-{up,test,...}
    #   mkModeScripts "tournament" "default" 7 8
    #
    # The `flavor` selects which microvm runner-dir is baked into `up`/`restart`
    # (different hypervisor + virtio-gpu config). `--cluster <name>` is what
    # partitions VM leases, state dir, and history. The default flavor uses
    # `<mode>` as the cluster name (backwards compatible); other flavors use
    # `<mode>-<flavor>` so they never collide with the default flavor or each
    # other when run in parallel.
    mkModeScripts = mode: flavorName: count: players: let
      isDefault = flavorName == "default";
      scriptName =
        if isDefault
        then mode
        else "${mode}-${flavorName}";
      clusterName = scriptName;
      flavor = flavors.${flavorName};
      flavorRunnersDir = mkRunnersDir flavorName flavor count;
      c = "--cluster ${clusterName}";
    in {
      "cluster-${scriptName}-up" = mkWrapper "${scriptName}-up" count "${c} up --runners-dir ${flavorRunnersDir}";
      "cluster-${scriptName}-down" = mkWrapper "${scriptName}-down" count "${c} down";
      "cluster-${scriptName}-restart" = mkWrapper "${scriptName}-restart" count "${c} restart --runners-dir ${flavorRunnersDir}";
      "cluster-${scriptName}-deploy" = mkWrapper "${scriptName}-deploy" count "${c} deploy";
      "cluster-${scriptName}-run" = mkWrapper "${scriptName}-run" count "${c} run";
      "cluster-${scriptName}-stop-game" = mkWrapper "${scriptName}-stop-game" count "${c} stop-game";
      "cluster-${scriptName}-logs" = mkWrapper "${scriptName}-logs" count "${c} logs";
      "cluster-${scriptName}-test" = mkLanTestWrapper scriptName clusterName count players;
    };

    # Generate scripts for every flavor of a mode in one merge.
    mkAllFlavorScripts = mode: count: players:
      lib.foldl' (acc: flavorName: acc // (mkModeScripts mode flavorName count players))
      {} (lib.attrNames flavors);

    cluster1v1WaylandTest = pkgs.writeShellScript "cluster-1v1-test-wayland" ''
      if [ -z "''${WAYLAND_DISPLAY:-}" ] && [ -z "''${WAYLAND_SOCKET:-}" ]; then
        echo "[cluster-1v1-test-wayland] Wayland session required: set WAYLAND_DISPLAY or WAYLAND_SOCKET." >&2
        exit 1
      fi

      export WINIT_UNIX_BACKEND=wayland
      export XKB_CONFIG_ROOT=${pkgs.xkeyboard_config}/share/X11/xkb
      exec ${pkgs.nix}/bin/nix develop -c \
        ${ctl} --vm-count 1 ${credsFlag} --cluster 1v1 test --players 2 --display headless "$@"
    '';
  in {
    inherit network linuxVMs;

    scripts =
      # -- Mode-specific scripts (one set per flavor declared in steampipe.toml) --
      (mkAllFlavorScripts "1v1" 1 2)
      // {
        "cluster-1v1-steam-test" = mkWrapper "1v1-steam-test" 1 "--cluster 1v1 test --network steam --players 2";
        "cluster-1v1-steam-login" = mkSteamLoginWrapper "1v1-steam-login" 1 "--cluster 1v1 steam-login --login-runners-dir ${loginVMRunnersDir}";
        "cluster-1v1-steam-guard" = mkSteamLoginWrapper "1v1-steam-guard" 1 "--cluster 1v1 steam-guard";
        "cluster-1v1-test-wayland" = cluster1v1WaylandTest;
      }
      // (mkAllFlavorScripts "tournament" vmCount 8)
      // {
        "cluster-tournament-steam-test" = mkWrapper "tournament-steam-test" vmCount "--cluster tournament test --network steam --players 8";
      }
      # -- Shared utilities (full cluster) --
      // {
        cluster-status = mkWrapper "status" vmCount "status";
        cluster-doctor = mkWrapper "doctor" vmCount "doctor";
        cluster-watch = mkWrapper "watch" vmCount "watch";
        cluster-logs = mkWrapper "logs" vmCount "logs";
        cluster-logs-follow = mkWrapper "logs-follow" vmCount "logs --follow";
        cluster-history = mkWrapper "history" vmCount "history";
        cluster-accounts = mkWrapper "accounts" vmCount "accounts";
        cluster-screenshot = mkWrapper "screenshot" vmCount "screenshot";
        cluster-deploy = mkWrapper "deploy" vmCount "deploy";
        cluster-deploy-verify = mkWrapper "deploy-verify" vmCount "deploy --verify";
        cluster-history-trends = mkWrapper "history-trends" vmCount "history trends";
        cluster-history-flaky = mkWrapper "history-flaky" vmCount "history flaky";
        cluster-bisect = mkWrapper "bisect" vmCount "bisect";
      }
      # -- Steam management (full cluster) --
      // {
        cluster-steam-start = mkWrapper "steam-start" vmCount "steam-start";
        cluster-steam-check = mkWrapper "steam-check" vmCount "steam-check --runners-dir ${vmRunnersDir}";
        cluster-steam-login = mkSteamLoginWrapper "steam-login" vmCount "steam-login --login-runners-dir ${loginVMRunnersDir}";
        cluster-steam-guard = mkSteamLoginWrapper "steam-guard" vmCount "steam-guard";
      }
      # System-level login persist: boots login VMs with virtiofs-mounted loginStateDir,
      # logs in, and Steam writes session files directly to host.
      // lib.optionalAttrs nixosIntegration {
        cluster-steam-login-persist =
          mkWrapper "steam-login-persist" vmCount
          "--credentials ${nixosCredentials} steam-login --login-runners-dir ${loginVMRunnersDir}";
      }
      # -- Network simulation (full cluster, requires sudo) --
      // {
        cluster-netem = mkWrapper "netem" vmCount "netem";
        cluster-netem-show = mkWrapper "netem-show" vmCount "netem-show";
        cluster-netem-reset = mkWrapper "netem-reset" vmCount "netem-reset";
      }
      # -- Snapshots --
      // {
        cluster-snapshot-save = mkWrapper "snapshot-save" vmCount "snapshot-save";
        cluster-snapshot-restore = mkWrapper "snapshot-restore" vmCount "snapshot-restore";
        cluster-snapshot-list = mkWrapper "snapshot-list" vmCount "snapshot-list";
        cluster-snapshot-delete = mkWrapper "snapshot-delete" vmCount "snapshot-delete";
      };
  }
