{
  clusterCtl,
  vmRunnersDir,
  vmCount,
  defaultUser,
  defaultSshKey,
}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.steampipe-warm-timer;
  userHome = config.users.users.${cfg.user}.home or "/home/${cfg.user}";
  warmCommand = pkgs.writeShellScript "steampipe-steam-warm" ''
    exec ${lib.escapeShellArgs ([
        "${clusterCtl}/bin/cluster-ctl"
        "--vm-count"
        (toString vmCount)
        "--ssh-key"
        cfg.sshKey
        "steam"
        "warm"
        "--runners-dir"
        (toString vmRunnersDir)
      ]
      ++ cfg.extraArgs)}
  '';
in {
  options.services.steampipe-warm-timer = {
    enable = lib.mkEnableOption "weekly cluster-ctl steam warm timer";

    user = lib.mkOption {
      type = lib.types.str;
      default = defaultUser;
      description = ''
        Host user that runs the warm command. Must own the configured SSH
        key and the Steam login state used by the cluster.
      '';
    };

    sshKey = lib.mkOption {
      type = lib.types.str;
      default = defaultSshKey;
      description = ''
        SSH private key path passed to cluster-ctl via --ssh-key. This is a
        host filesystem path, not a Nix store path; do not point it at secret
        material imported into the store.
      '';
    };

    onCalendar = lib.mkOption {
      type = lib.types.str;
      default = "weekly";
      description = ''
        systemd OnCalendar= expression. The default runs once per week.
      '';
    };

    randomizedDelaySec = lib.mkOption {
      type = lib.types.str;
      default = "1h";
      description = "RandomizedDelaySec= to spread load across hosts.";
    };

    extraArgs = lib.mkOption {
      type = lib.types.listOf lib.types.str;
      default = [];
      description = ''
        Extra arguments appended to cluster-ctl steam warm. Useful for target
        selection or flags such as --cluster when running outside the default
        cluster context.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = builtins.hasAttr cfg.user config.users.users;
        message = "services.steampipe-warm-timer.user '${cfg.user}' must be a configured user; check services.steampipe-cluster.tapOwner and your users.users configuration.";
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
      description = "cluster-ctl steam warm (one-shot; expired Steam Guard sessions fail fast and require operator re-login)";
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
