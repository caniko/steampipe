{
  lib,
  microvm,
  nixpkgs,
  pkgs,
  self,
  system,
}: let
  steampipeModule = {...}: {
    imports = [
      microvm.nixosModules.host
      ../module.nix
    ];
    _module.args = {
      inherit nixpkgs microvm;
      steampipe = self;
    };
  };

  fakeSshAuthorizedKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForSteampipeEvalChecksOnly steampipe-test";

  warmTimerModule = import ../lib/warm-timer.nix {
    clusterCtl = pkgs.writeShellScriptBin "cluster-ctl" "exit 0";
    vmRunnersDir = pkgs.runCommand "warm-timer-runners" {} "mkdir -p $out";
    vmCount = 7;
    defaultUser = "cluster";
    defaultSshKey = "/home/cluster/.ssh/cluster_key";
  };

  warmTimerDisabledModule = import ../lib/warm-timer.nix {
    clusterCtl = pkgs.writeShellScriptBin "cluster-ctl" "exit 0";
    vmRunnersDir = pkgs.runCommand "warm-timer-runners-disabled" {} "mkdir -p $out";
    vmCount = 7;
    defaultUser = "cluster";
    defaultSshKey = "/home/cluster/.ssh/cluster_key";
  };

  clusterUserModule = {
    users.users.cluster = {
      isNormalUser = true;
      home = "/home/cluster";
    };
  };

  baseClusterConfig = {
    services.steampipe-cluster = {
      enable = true;
      vmCount = 2;
      tapOwner = "cluster";
      tapOwnerUid = 1000;
    };
  };

  mkSystem = modules:
    nixpkgs.lib.nixosSystem {
      inherit system;
      modules =
        [
          {
            system.stateVersion = "26.05";
          }
        ]
        ++ modules;
    };

  warmTimerEval = mkSystem [
    warmTimerModule
    clusterUserModule
    {
      services.steampipe-warm-timer.enable = true;
    }
  ];

  warmTimerDisabledEval = mkSystem [
    warmTimerDisabledModule
  ];

  hostWarmTimerEval = mkSystem [
    steampipeModule
    ../modules/warm-timer.nix
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.runners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
      };
      services.steampipe-warm-timer.enable = true;
    }
  ];

  hostWarmTimerMissingRunnersEval = mkSystem [
    steampipeModule
    ../modules/warm-timer.nix
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-warm-timer.enable = true;
    }
  ];

  moduleLoginRunnersEnabledEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.loginRunners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
      };
    }
  ];

  moduleLoginRunnersEmptyKeyEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.loginRunners.enable = true;
    }
  ];

  moduleLoginRunnersDisabledEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
  ];

  moduleRunnersEmptyKeyEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.runners.enable = true;
    }
  ];

  moduleRunnersEnabledEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.runners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
      };
    }
  ];

  moduleCloudHypervisorRunnersEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.runners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
        hypervisor = "cloud-hypervisor";
        graphics = false;
      };
    }
  ];

  moduleQemuLoginRunnersEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.loginRunners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
        hypervisor = "qemu";
      };
    }
  ];

  moduleCrosvmDeprecationEval = mkSystem [
    steampipeModule
    clusterUserModule
    baseClusterConfig
    {
      services.steampipe-cluster.loginRunners = {
        enable = true;
        sshAuthorizedKey = fakeSshAuthorizedKey;
        hypervisor = "crosvm";
      };
    }
  ];

  boolString = value:
    if value
    then "true"
    else "false";

  memoryBudget = args: import ../lib/microvm-memory-budget.nix args;
  qemuLoginBudget = memoryBudget {
    hypervisor = "qemu";
    shareSteamState = true;
  };
  qemuRuntimeBudget = memoryBudget {
    hypervisor = "qemu";
    shareSteamState = false;
  };
  cloudLoginBudget = memoryBudget {
    hypervisor = "cloud-hypervisor";
    shareSteamState = true;
  };
  cloudRuntimeBudget = memoryBudget {
    hypervisor = "cloud-hypervisor";
    shareSteamState = false;
  };
  crosvmRuntimeBudget = memoryBudget {
    hypervisor = "crosvm";
    shareSteamState = false;
  };
in
  lib.optionalAttrs pkgs.stdenv.isLinux {
    warm-timer-eval =
      pkgs.runCommand "warm-timer-eval" {
        enabled =
          if warmTimerEval.config.services.steampipe-warm-timer.enable
          then "true"
          else "false";
        disabledByDefault =
          if warmTimerDisabledEval.config.services.steampipe-warm-timer.enable
          then "false"
          else "true";
        serviceUser = warmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.User;
        timerOnCalendar = warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.OnCalendar;
        timerPersistent =
          if warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.Persistent
          then "true"
          else "false";
        timerRandomizedDelaySec = warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.RandomizedDelaySec;
      } ''
        test "$enabled" = true
        test "$disabledByDefault" = true
        test "$serviceUser" = cluster
        test "$timerOnCalendar" = weekly
        test "$timerPersistent" = true
        test "$timerRandomizedDelaySec" = 1h
        touch "$out"
      '';

    host-warm-timer-eval =
      pkgs.runCommand "host-warm-timer-eval" {
        timerOnCalendar = hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.OnCalendar;
        timerPersistent =
          if hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.Persistent
          then "true"
          else "false";
        timerRandomizedDelaySec = hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.RandomizedDelaySec;
        serviceUser = hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.User;
        serviceHome = builtins.elemAt hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.Environment 0;
        warmCommand = hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.ExecStart;
      } ''
        test "$timerOnCalendar" = weekly
        test "$timerPersistent" = true
        test "$timerRandomizedDelaySec" = 1h
        test "$serviceUser" = cluster
        test "$serviceHome" = HOME=/home/cluster
        warmCommandText="$(cat "$warmCommand")"
        case "$warmCommandText" in
          *"--runners-dir"*) echo "host warm timer should rely on module.json, not --runners-dir" >&2; exit 1 ;;
        esac
        case "$warmCommandText" in
          *"cluster-ctl"*"--vm-count 2"*"--ssh-key /var/lib/steampipe/ssh/cluster_key"*"steam warm"*) ;;
          *) echo "unexpected warm command: $warmCommandText" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

    host-warm-timer-missing-runners-eval =
      pkgs.runCommand "host-warm-timer-missing-runners-eval" {
        assertionMessage = let
          failed =
            lib.findFirst
            (assertion: !assertion.assertion)
            null
            hostWarmTimerMissingRunnersEval.config.assertions;
        in
          if failed == null
          then ""
          else failed.message;
      } ''
        case "$assertionMessage" in
          *"services.steampipe-warm-timer.enable requires services.steampipe-cluster.runners.enable"*) ;;
          *) echo "expected runners.enable assertion not seen: $assertionMessage" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

    module-loginrunners-eval =
      pkgs.runCommand "module-loginrunners-eval" {
        enabledJson = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/module.json".text;
        disabledJson = moduleLoginRunnersDisabledEval.config.environment.etc."steampipe/module.json".text;
        loginRunnersDir = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/login-runners".source;
      } ''
                ${pkgs.python3}/bin/python - <<'PY'
        import json
        import os

        enabled = json.loads(os.environ["enabledJson"])
        disabled = json.loads(os.environ["disabledJson"])

        assert enabled["schemaVersion"] == 2
        assert enabled["vmCount"] == 2
        assert enabled["vmUser"] == "cluster"
        assert enabled["loginRunnersDir"] == "/etc/steampipe/login-runners"
        assert enabled["sshKey"] == "/var/lib/steampipe/ssh/cluster_key"

        assert disabled["schemaVersion"] == 2
        assert disabled["vmCount"] == 2
        assert disabled["vmUser"] == "cluster"
        assert "loginRunnersDir" not in disabled
        assert "sshKey" not in disabled
        PY

                test -x "$loginRunnersDir/vm-1/bin/microvm-run"
                test -x "$loginRunnersDir/vm-2/bin/microvm-run"
                test ! -e "$loginRunnersDir/vm-3"
                touch "$out"
      '';

    module-runners-eval =
      pkgs.runCommand "module-runners-eval" {
        enabledJson = moduleRunnersEnabledEval.config.environment.etc."steampipe/module.json".text;
        disabledJson = moduleLoginRunnersDisabledEval.config.environment.etc."steampipe/module.json".text;
        runnersDir = moduleRunnersEnabledEval.config.environment.etc."steampipe/runners".source;
        runnersEtcEnabled =
          if moduleRunnersEnabledEval.config.environment.etc ? "steampipe/runners"
          then "true"
          else "false";
        runnersEtcDisabled =
          if moduleLoginRunnersDisabledEval.config.environment.etc ? "steampipe/runners"
          then "true"
          else "false";
      } ''
                ${pkgs.python3}/bin/python - <<'PY'
        import json
        import os

        enabled = json.loads(os.environ["enabledJson"])
        disabled = json.loads(os.environ["disabledJson"])

        assert enabled["schemaVersion"] == 2
        assert enabled["vmCount"] == 2
        assert enabled["vmUser"] == "cluster"
        assert enabled["runnersDir"] == "/etc/steampipe/runners"
        assert enabled["sshKey"] == "/var/lib/steampipe/ssh/cluster_key"
        assert "loginRunnersDir" not in enabled

        assert disabled["schemaVersion"] == 2
        assert disabled["vmCount"] == 2
        assert disabled["vmUser"] == "cluster"
        assert "runnersDir" not in disabled
        assert "sshKey" not in disabled
        PY

                test "$runnersEtcEnabled" = true
                test "$runnersEtcDisabled" = false
                test -x "$runnersDir/vm-1/bin/microvm-run"
                test -x "$runnersDir/vm-2/bin/microvm-run"
                test ! -e "$runnersDir/vm-3"
                touch "$out"
      '';

    module-cloud-hypervisor-runners-eval =
      pkgs.runCommand "module-cloud-hypervisor-runners-eval" {
        enabledJson = moduleCloudHypervisorRunnersEval.config.environment.etc."steampipe/module.json".text;
        runnersDir = moduleCloudHypervisorRunnersEval.config.environment.etc."steampipe/runners".source;
        runnerScript = "${moduleCloudHypervisorRunnersEval.config.environment.etc."steampipe/runners".source}/vm-1/bin/microvm-run";
        runnersHypervisor = moduleCloudHypervisorRunnersEval.config.services.steampipe-cluster.runners.hypervisor;
        runnersGraphics =
          if moduleCloudHypervisorRunnersEval.config.services.steampipe-cluster.runners.graphics
          then "true"
          else "false";
        defaultHypervisor = moduleRunnersEnabledEval.config.services.steampipe-cluster.runners.hypervisor;
      } ''
                ${pkgs.python3}/bin/python - <<'PY'
        import json
        import os

        enabled = json.loads(os.environ["enabledJson"])

        assert enabled["schemaVersion"] == 2
        assert enabled["vmCount"] == 2
        assert enabled["vmUser"] == "cluster"
        assert enabled["runnersDir"] == "/etc/steampipe/runners"
        assert enabled["sshKey"] == "/var/lib/steampipe/ssh/cluster_key"
        assert "loginRunnersDir" not in enabled
        PY

                test "$runnersHypervisor" = cloud-hypervisor
                test "$runnersGraphics" = false
                test "$defaultHypervisor" = cloud-hypervisor
                test -x "$runnersDir/vm-1/bin/microvm-run"
                grep -q cloud-hypervisor "$runnerScript"
                touch "$out"
      '';

    module-qemu-loginrunners-eval =
      pkgs.runCommand "module-qemu-loginrunners-eval" {
        loginRunnersDir = moduleQemuLoginRunnersEval.config.environment.etc."steampipe/login-runners".source;
        runnerScript = "${moduleQemuLoginRunnersEval.config.environment.etc."steampipe/login-runners".source}/vm-1/bin/microvm-run";
        loginHypervisor = moduleQemuLoginRunnersEval.config.services.steampipe-cluster.loginRunners.hypervisor;
        defaultLoginHypervisor = moduleLoginRunnersEnabledEval.config.services.steampipe-cluster.loginRunners.hypervisor;
        loginGraphics =
          if moduleQemuLoginRunnersEval.config.services.steampipe-cluster.loginRunners.graphics
          then "true"
          else "false";
      } ''
        test "$loginHypervisor" = qemu
        test "$defaultLoginHypervisor" = qemu
        test "$loginGraphics" = false
        test -x "$loginRunnersDir/vm-1/bin/microvm-run"
        grep -q qemu-system-x86_64 "$runnerScript"
        grep -q -- "-nographic" "$runnerScript"
        if grep -Eq -- "-device (virtio-vga-gl|virtio-gpu-gl-pci)" "$runnerScript"; then
          echo "qemu login runner unexpectedly contains a virgl virtio-gpu device: $runnerScript" >&2
          exit 1
        fi
        if grep -q "VK_DRIVER_FILES" "$runnerScript"; then
          echo "qemu login runner unexpectedly exports VK_DRIVER_FILES: $runnerScript" >&2
          exit 1
        fi
        touch "$out"
      '';

    module-crosvm-deprecation-warning =
      pkgs.runCommand "module-crosvm-deprecation-warning" {
        warningsText = builtins.concatStringsSep "\n" moduleCrosvmDeprecationEval.config.warnings;
      } ''
        case "$warningsText" in
          *"services.steampipe-cluster.loginRunners.hypervisor = \"crosvm\" is deprecated"*) ;;
          *) echo "expected crosvm deprecation warning not seen: $warningsText" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

    module-memory-budgets-eval =
      pkgs.runCommand "module-memory-budgets-eval" {
        qemuLoginBalloon = boolString qemuLoginBudget.balloon;
        qemuLoginDeflateOnOOM = boolString qemuLoginBudget.deflateOnOOM;
        qemuLoginInitialBalloonMem = toString qemuLoginBudget.initialBalloonMem;
        qemuRuntimeBalloon = boolString qemuRuntimeBudget.balloon;
        qemuRuntimeDeflateOnOOM = boolString qemuRuntimeBudget.deflateOnOOM;
        qemuRuntimeInitialBalloonMem = toString qemuRuntimeBudget.initialBalloonMem;
        cloudLoginBalloon = boolString cloudLoginBudget.balloon;
        cloudLoginDeflateOnOOM = boolString cloudLoginBudget.deflateOnOOM;
        cloudLoginInitialBalloonMem = toString cloudLoginBudget.initialBalloonMem;
        cloudRuntimeBalloon = boolString cloudRuntimeBudget.balloon;
        cloudRuntimeDeflateOnOOM = boolString cloudRuntimeBudget.deflateOnOOM;
        cloudRuntimeInitialBalloonMem = toString cloudRuntimeBudget.initialBalloonMem;
        crosvmRuntimeBalloon = boolString crosvmRuntimeBudget.balloon;
        crosvmRuntimeDeflateOnOOM = boolString crosvmRuntimeBudget.deflateOnOOM;
        crosvmRuntimeInitialBalloonMem = toString crosvmRuntimeBudget.initialBalloonMem;
      } ''
        test "$qemuLoginBalloon" = true
        test "$qemuLoginDeflateOnOOM" = true
        test "$qemuLoginInitialBalloonMem" = 0
        test "$qemuRuntimeBalloon" = true
        test "$qemuRuntimeDeflateOnOOM" = true
        test "$qemuRuntimeInitialBalloonMem" = 0

        test "$cloudLoginBalloon" = true
        test "$cloudLoginDeflateOnOOM" = true
        test "$cloudLoginInitialBalloonMem" = 1024
        test "$cloudRuntimeBalloon" = true
        test "$cloudRuntimeDeflateOnOOM" = true
        test "$cloudRuntimeInitialBalloonMem" = 512

        test "$crosvmRuntimeBalloon" = false
        test "$crosvmRuntimeDeflateOnOOM" = false
        test "$crosvmRuntimeInitialBalloonMem" = 0

        touch "$out"
      '';

    # Regression guard: loginRunners must not silently build VMs with an
    # empty sshAuthorizedKey default under pure flake evaluation.
    module-loginrunners-empty-key-asserts =
      pkgs.runCommand "module-loginrunners-empty-key-asserts" {
        assertionMessage = let
          failed =
            lib.findFirst
            (assertion: !assertion.assertion)
            null
            moduleLoginRunnersEmptyKeyEval.config.assertions;
        in
          if failed == null
          then ""
          else failed.message;
      } ''
        case "$assertionMessage" in
          *"loginRunners.enable is true but"*"sshAuthorizedKey is empty"*) ;;
          *) echo "expected loginRunners empty sshAuthorizedKey assertion not seen: $assertionMessage" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

    # Regression guard: legacy runners must not silently build VMs with an
    # empty sshAuthorizedKey default under pure flake evaluation.
    module-runners-empty-key-asserts =
      pkgs.runCommand "module-runners-empty-key-asserts" {
        assertionMessage = let
          failed =
            lib.findFirst
            (assertion: !assertion.assertion)
            null
            moduleRunnersEmptyKeyEval.config.assertions;
        in
          if failed == null
          then ""
          else failed.message;
      } ''
        case "$assertionMessage" in
          *"runners.enable is true but"*"sshAuthorizedKey is empty"*) ;;
          *) echo "expected runners empty sshAuthorizedKey assertion not seen: $assertionMessage" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

    # Regression guard: the empty-key assertion must not reject a configured
    # loginRunners.sshAuthorizedKey on the normal happy path.
    module-loginrunners-with-key-evals =
      pkgs.runCommand "module-loginrunners-with-key-evals" {
        enabled =
          if moduleLoginRunnersEnabledEval.config.services.steampipe-cluster.loginRunners.enable
          then "true"
          else "false";
        loginRunnersDir = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/login-runners".source;
      } ''
        test "$enabled" = true
        test -x "$loginRunnersDir/vm-1/bin/microvm-run"
        touch "$out"
      '';

    module-cluster-ctl-install-eval =
      pkgs.runCommand "module-cluster-ctl-install-eval" {
        packagesText = builtins.concatStringsSep " " (map (p: p.pname or "") moduleLoginRunnersEnabledEval.config.environment.systemPackages);
      } ''
        case "$packagesText" in
          *cluster-ctl*) ;;
          *) echo "cluster-ctl not in environment.systemPackages: $packagesText" >&2; exit 1 ;;
        esac
        touch "$out"
      '';

  }
