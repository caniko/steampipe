# services.steampipe-warm-timer: flake-level NixOS module.
#
# Reads host config from services.steampipe-cluster and refreshes persisted
# SteamClient sessions without booting runtime VMs.
{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.steampipe-warm-timer;
  clusterCfg = config.services.steampipe-cluster;
  userHome = config.users.users.${cfg.user}.home or "/home/${cfg.user}";
  warmCommand = pkgs.writeShellScript "steampipe-steam-warm" ''
    exec ${lib.escapeShellArgs ([
        "${clusterCfg.package}/bin/cluster-ctl"
        "--non-interactive"
        "--no-gui"
        "steam"
        "refresh"
      ]
      ++ cfg.extraArgs)}
  '';
in {
  options.services.steampipe-warm-timer = {
    enable = lib.mkEnableOption "weekly cluster-ctl steam refresh timer (host-level)";

    user = lib.mkOption {
      type = lib.types.str;
      default = clusterCfg.tapOwner;
      defaultText = lib.literalExpression "config.services.steampipe-cluster.tapOwner";
      description = "Host user that runs the warm command.";
    };

    sshKey = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/steampipe/ssh/cluster_key";
      description = "Deprecated compatibility option; steam refresh does not use SSH.";
    };

    onCalendar = lib.mkOption {
      type = lib.types.str;
      default = "weekly";
      description = "systemd OnCalendar= expression.";
    };

    randomizedDelaySec = lib.mkOption {
      type = lib.types.str;
      default = "1h";
      description = "RandomizedDelaySec= jitter.";
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = "Extra arguments appended to cluster-ctl steam refresh.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = clusterCfg.enable;
        message = "services.steampipe-warm-timer.enable requires services.steampipe-cluster.enable = true.";
      }
      {
        assertion = builtins.hasAttr cfg.user config.users.users;
        message = "services.steampipe-warm-timer.user '${cfg.user}' must be a configured user.";
      }
    ];

    systemd.timers.steampipe-steam-warm = {
      description = "Refresh Steam session tokens";
      wantedBy = ["timers.target"];
      timerConfig = {
        OnCalendar = cfg.onCalendar;
        Persistent = true;
        RandomizedDelaySec = cfg.randomizedDelaySec;
      };
    };

    systemd.services.steampipe-steam-warm = {
      description = "cluster-ctl steam refresh (one-shot)";
      after = [
        "NetworkManager-wait-online.service"
        "network-online.target"
      ];
      wants = [
        "NetworkManager-wait-online.service"
        "network-online.target"
      ];
      serviceConfig = {
        Type = "oneshot";
        User = cfg.user;
        RuntimeDirectory = "steampipe-steam-warm";
        RuntimeDirectoryMode = "0700";
        UMask = "0077";
        Environment = [
          "HOME=${userHome}"
          "XDG_RUNTIME_DIR=/run/steampipe-steam-warm"
        ];
        ExecStart = warmCommand;
      };
    };
  };
}
