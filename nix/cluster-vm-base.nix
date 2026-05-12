# Base NixOS configuration for steampipe-managed test-cluster microVMs.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.steampipe.clusterVm;
  homeDir = "/home/${cfg.user}";
in {
  options.steampipe.clusterVm = {
    user = lib.mkOption {
      type = lib.types.str;
      default = "cluster";
      description = "Normal user account used for SSH deployment and VM diagnostics.";
    };

    sshAuthorizedKeys = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "SSH public keys authorized for the cluster VM user.";
    };

    enableSteam = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Enable Steam client tooling in the VM.";
    };

    useSystemdStage1 = lib.mkOption {
      type = lib.types.nullOr lib.types.bool;
      default = null;
      description = ''
        Override boot.initrd.systemd.enable for this VM. Leave null to preserve
        the hypervisor-specific default from microvm.nix.
      '';
    };
  };

  config = lib.mkMerge [
    {
      # Use classic interface names (eth0) instead of predictable naming.
      boot.kernelParams = ["net.ifnames=0"];

      networking = {
        useDHCP = false;
        firewall.enable = false;
        # Static IP is set per VM in the cluster definition.
        defaultGateway = {
          address = "10.0.100.254";
          interface = "eth0";
        };
        nameservers = ["10.0.100.254" "1.1.1.1"];
      };

      services.openssh = {
        enable = true;
        settings = {
          PasswordAuthentication = false;
          PermitRootLogin = "no";
        };
      };

      security.sudo.wheelNeedsPassword = false;

      users.users.${cfg.user} = {
        isNormalUser = true;
        home = homeDir;
        extraGroups = ["input" "render" "seat" "systemd-journal" "video" "wheel"];
        openssh.authorizedKeys.keys = cfg.sshAuthorizedKeys;
      };

      # Ensure home dir ownership after volume mount (mkfs creates root-owned root).
      systemd.tmpfiles.rules = [
        "d ${homeDir} 0755 ${cfg.user} users -"
      ];

      nixpkgs.config.allowUnfree = lib.mkIf cfg.enableSteam true;
      programs.steam.enable = lib.mkIf cfg.enableSteam true;

      # sway: default Wayland compositor for Steam runtime + VNC Steam Guard
      # login. Visual testing (grim screenshots, wayvnc, swaymsg readiness
      # probes, fixture golden regen) all require sway specifically — wlroots
      # IPC is the only readiness interface the harness knows.
      # weston: experimental alternate compositor, kept for parity testing only;
      # not supported by the visual capture pipeline.
      services.seatd.enable = true;

      environment.sessionVariables = {
        LIBSEAT_BACKEND = "seatd";
        XKB_CONFIG_ROOT = "${pkgs.xkeyboard_config}/share/X11/xkb";
      };

      environment.systemPackages =
        (with pkgs; [
          htop
          (writeShellScriptBin "weston" ''
            if [ -n "''${XDG_RUNTIME_DIR:-}" ]; then
              chmod 700 "$XDG_RUNTIME_DIR" 2>/dev/null || true
            fi

            has_backend=0
            for arg in "$@"; do
              case "$arg" in
                --backend|--backend=*)
                  has_backend=1
                  break
                  ;;
              esac
            done

            unset WAYLAND_DISPLAY WAYLAND_SOCKET DISPLAY
            if [ "$has_backend" = 1 ]; then
              exec ${weston}/bin/weston "$@"
            fi
            exec ${weston}/bin/weston --backend=drm "$@"
          '')
          # nixpkgs pins sway 1.11 here; that release exposes the get_outputs /
          # get_tree IPC fields this repo consumes for compositor readiness.
          sway
          grim
          foot
          wf-recorder
          xkeyboard_config
          tmux
          wayvnc
          wl-clipboard
          wtype
        ])
        ++ lib.optionals cfg.enableSteam (with pkgs; [
          steamcmd
        ]);

      system.stateVersion = "25.05";
    }

    (lib.mkIf (cfg.useSystemdStage1 != null) {
      boot.initrd.systemd.enable = cfg.useSystemdStage1;
    })

    (lib.mkIf (cfg.useSystemdStage1 == true) {
      # microvm.nix's crosvm systemd-stage-1 default is off because the store
      # disk mount needs filesystem modules available before sysroot mounts.
      boot.initrd.kernelModules = ["erofs" "squashfs"];

      fileSystems."/" = {
        device = lib.mkForce "tmpfs";
        fsType = lib.mkForce "tmpfs";
        neededForBoot = lib.mkForce false;
      };

      boot.initrd.systemd.root = null;
      boot.initrd.systemd.mounts = [
        {
          what = "tmpfs";
          where = "/sysroot";
          type = "tmpfs";
          options = "size=50%,mode=0755";
          unitConfig.DefaultDependencies = false;
          requiredBy = ["initrd-root-fs.target"];
          before = ["initrd-root-fs.target"];
        }
      ];

      boot.initrd.systemd.services.steampipe-sysroot-mountpoints = {
        description = "Create steampipe microVM sysroot mount points";
        requires = ["sysroot.mount"];
        after = ["sysroot.mount"];
        before = [
          "sysroot-nix-store.mount"
          "sysroot-run.mount"
          "initrd-fs.target"
        ];
        requiredBy = [
          "sysroot-nix-store.mount"
          "sysroot-run.mount"
        ];
        unitConfig.DefaultDependencies = false;
        serviceConfig = {
          Type = "oneshot";
          ExecStart = "/bin/mkdir -p /sysroot/nix/store /sysroot/run";
        };
      };
    })
  ];
}
