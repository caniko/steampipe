# services.steampipe-warm-timer: flake-level NixOS module.
#
# Reads runner dir, SSH key, vmCount, and vmUser from
# services.steampipe-cluster instead of injected arguments. This is the
# host-level counterpart to (mkTestCluster {...}).warmTimerModule for projects
# that consume steampipe via NixOS module only.
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
        "--vm-count"
        (toString clusterCfg.vmCount)
        "--ssh-key"
        cfg.sshKey
        "steam"
        "warm"
      ]
      ++ cfg.extraArgs)}
  '';
in {
  options.services.steampipe-warm-timer = {
    enable = lib.mkEnableOption "weekly cluster-ctl steam warm timer (host-level)";

    user = lib.mkOption {
      type = lib.types.str;
      default = clusterCfg.tapOwner;
      defaultText = lib.literalExpression "config.services.steampipe-cluster.tapOwner";
      description = "Host user that runs the warm command.";
    };

    sshKey = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/steampipe/ssh/cluster_key";
      description = "SSH private key path passed to cluster-ctl via --ssh-key.";
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
      description = "Extra arguments appended to cluster-ctl steam warm.";
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = clusterCfg.enable;
        message = "services.steampipe-warm-timer.enable requires services.steampipe-cluster.enable = true.";
      }
      {
        assertion = clusterCfg.runners.enable or false;
        message = "services.steampipe-warm-timer.enable requires services.steampipe-cluster.runners.enable = true so cluster-ctl can resolve runnersDir from /etc/steampipe/module.json. (Decision D3: no silent disable; configure runners.enable or remove the warm timer.)";
      }
      {
        assertion = builtins.hasAttr cfg.user config.users.users;
        message = "services.steampipe-warm-timer.user '${cfg.user}' must be a configured user.";
      }
    ];

    systemd.timers.steampipe-steam-warm = {
      description = "Refresh Steam session tokens on cluster VMs";
      wantedBy = ["timers.target"];
      timerConfig = {
        OnCalendar = cfg.onCalendar;
        Persistent = true;
        RandomizedDelaySec = cfg.randomizedDelaySec;
      };
    };

    systemd.services.steampipe-steam-warm = {
      description = "cluster-ctl steam warm (one-shot)";
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
        Environment = [
          "HOME=${userHome}"
        ];
        ExecStart = warmCommand;
      };
    };
  };
}
