{
  config,
  lib,
  pkgs,
  steampipe ? null,
  nixpkgs ? null,
  ...
}: let
  cfg = config.services.steampipe-cluster;
  vmNames = builtins.genList (i: "vm-${toString (i + 1)}") cfg.vmCount;
  tapNames = map (n: "tap-${n}") vmNames;
  vmUser = "cluster";
  loginStateOwner =
    if cfg.tapOwner != null
    then cfg.tapOwner
    else "root";
  loginRunnerSshDir = "/var/lib/steampipe/ssh";
  loginRunnerSshKey = "${loginRunnerSshDir}/cluster_key";
  loginRunnerAuthorizedKeysDir = "/var/lib/steampipe/ssh-authorized";

  loginRunnersDir =
    (import ./lib/login-runners.nix {
      inherit pkgs lib nixpkgs steampipe;
    }) {
      inherit (cfg) vmCount subnet prefix;
      inherit vmUser;
      sshAuthorizedKeysPath = "/run/steampipe/authorized-keys/cluster_key.pub";
      sshAuthorizedKeysShare = {
        source = loginRunnerAuthorizedKeysDir;
        mountPoint = "/run/steampipe/authorized-keys";
      };
      hypervisor = cfg.loginRunners.hypervisor;
      graphics = cfg.loginRunners.graphics;
      memory = cfg.loginRunners.memory;
    };

  loginRunnerKeyScript = ''
    set -euo pipefail
    keydir=${lib.escapeShellArg loginRunnerSshDir}
    pubdir=${lib.escapeShellArg loginRunnerAuthorizedKeysDir}
    key="$keydir/cluster_key"

    ${pkgs.coreutils}/bin/install -d -m 0700 "$keydir"
    ${pkgs.coreutils}/bin/install -d -m 0755 "$pubdir"
    if [ ! -f "$key" ]; then
      ${pkgs.openssh}/bin/ssh-keygen -t ed25519 -N "" \
        -C "steampipe-loginrunners@$(${pkgs.coreutils}/bin/hostname)" \
        -f "$key"
    fi
    ${pkgs.coreutils}/bin/chmod 0600 "$key"
    ${pkgs.coreutils}/bin/chmod 0644 "$key.pub"
    ${pkgs.coreutils}/bin/install -m 0644 "$key.pub" "$pubdir/cluster_key.pub"
    ${lib.optionalString (cfg.tapOwner != null) ''
      ${pkgs.coreutils}/bin/chown -R ${cfg.tapOwner}:users "$keydir" "$pubdir"
    ''}
  '';

  nmConstants.tunMode.tap = "2";

  mkBridgeProfile = {
    name,
    address,
    prefix,
    dhcpRange,
  }: {
    connection = {
      id = name;
      type = "bridge";
      interface-name = name;
      autoconnect = "true";
    };
    bridge.stp = "false";
    ipv4 = {
      method = "shared";
      addresses = "${address}/${toString prefix}";
      shared-dhcp-range = dhcpRange;
    };
    ipv6.method = "disabled";
  };

  mkTapProfile = tap: {
    connection = {
      id = tap;
      type = "tun";
      interface-name = tap;
      autoconnect = "true";
      controller = cfg.bridge;
      port-type = "bridge";
    };
    tun =
      {
        mode = nmConstants.tunMode.tap;
        vnet-hdr = "true";
      }
      // lib.optionalAttrs (cfg.tapOwnerUid != null) {
        owner = toString cfg.tapOwnerUid;
      };
    bridge-port = {};
    ipv4.method = "disabled";
    ipv6.method = "disabled";
  };

  tapProfiles = builtins.listToAttrs (map (tap: {
      name = tap;
      value = mkTapProfile tap;
    })
    tapNames);
  # Script that reads agenix password files at runtime and generates credentials TOML
  credentialsGenScript = pkgs.writeShellScript "steampipe-gen-credentials" (''
      set -euo pipefail
      out="/etc/steampipe/credentials.toml"
      mkdir -p "$(dirname "$out")"
    ''
    + lib.concatStringsSep "" (lib.mapAttrsToList (name: creds: ''
        pass="$(cat ${lib.escapeShellArg (toString creds.steamPass)})"
        cat >> "$out" <<'TOML_HEADER'
        [vm."${name}"]
        steam_user = "${creds.steamUser}"
        TOML_HEADER
        printf 'steam_pass = "%s"\n\n' "$pass" >> "$out"
      '')
      cfg.accounts)
    + ''
      chmod 0400 "$out"
      ${lib.optionalString (cfg.tapOwner != null) "chown ${cfg.tapOwner} \"$out\""}
    '');
in {
  options.services.steampipe-cluster = {
    enable = lib.mkEnableOption "steampipe cluster networking";

    vmCount = lib.mkOption {
      type = lib.types.ints.between 1 32;
      default = 7;
      description = "Number of VM TAP devices to create.";
    };

    bridge = lib.mkOption {
      type = lib.types.str;
      default = "br-cluster";
      description = "Bridge interface name.";
    };

    subnet = lib.mkOption {
      type = lib.types.str;
      default = "10.0.100";
      description = "First three octets of the subnet (e.g. \"10.0.100\").";
    };

    prefix = lib.mkOption {
      type = lib.types.ints.between 1 32;
      default = 24;
      description = "CIDR prefix length.";
    };

    hostIp = lib.mkOption {
      type = lib.types.str;
      default = "10.0.100.254";
      description = "Host IP address on the bridge.";
    };

    natInterface = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        Deprecated. NetworkManager shared mode now manages NAT through
        the active default route instead of an explicit external interface.
      '';
    };

    tapOwner = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        User who owns the TAP devices. Required for unprivileged VM
        hypervisors (e.g. cloud-hypervisor) to open TAP devices.
      '';
    };

    tapOwnerUid = lib.mkOption {
      type = lib.types.nullOr lib.types.int;
      default = null;
      description = ''
        Numeric UID that owns the TAP devices created by NetworkManager.
        NetworkManager keyfiles require a numeric owner, not a username.
      '';
    };

    accounts = lib.mkOption {
      type = lib.types.attrsOf (lib.types.submodule {
        options = {
          steamUser = lib.mkOption {
            type = lib.types.str;
            description = "Steam account username.";
          };
          steamPass = lib.mkOption {
            type = lib.types.path;
            description = "Path to file containing the Steam account password (e.g. an agenix secret).";
          };
        };
      });
      default = {};
      description = ''
        Per-VM Steam account credentials, keyed by VM name (e.g. "vm-1").
        When set, a credentials TOML is generated for automated login.
        Login state directories are created under loginStateDir whenever
        the cluster service is enabled.
      '';
      example = lib.literalExpression ''
        {
          vm-1 = { steamUser = "testaccount1"; steamPass = "password1"; };
          vm-2 = { steamUser = "testaccount2"; steamPass = "password2"; };
        }
      '';
    };

    loginStateDir = lib.mkOption {
      type = lib.types.path;
      default = "/var/lib/steampipe/logins";
      description = ''
        Host directory where Steam login state is persisted.
        Each VM gets a subdirectory (e.g. vm-1/, vm-2/) containing
        the Steam state tree. Project-level flakes mount each VM's
        subdirectory writable at the VM user's ~/.local/share/Steam.
      '';
    };

    loginRunners = {
      enable = lib.mkEnableOption "host-level Steam login VM runners";

      hypervisor = lib.mkOption {
        type = lib.types.str;
        default = "crosvm";
        description = "Hypervisor for the login VMs.";
      };

      graphics = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Enable virtio-gpu in login VMs.";
      };

      memory = lib.mkOption {
        type = lib.types.ints.positive;
        default = 4096;
        description = "Memory per login VM in MiB. Steam GUI needs about 4 GiB.";
      };
    };
  };

  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = !cfg.loginRunners.enable || cfg.tapOwner != null;
        message = "services.steampipe-cluster.loginRunners.enable requires services.steampipe-cluster.tapOwner so cluster-ctl can read ${loginRunnerSshKey} without sudo.";
      }
      {
        assertion = !cfg.loginRunners.enable || steampipe != null;
        message = "services.steampipe-cluster.loginRunners.enable requires the steampipe flake self to be passed to nix/module.nix; use steampipe.nixosModules.default from the flake output.";
      }
      {
        assertion = !cfg.loginRunners.enable || nixpkgs != null;
        message = "services.steampipe-cluster.loginRunners.enable requires the nixpkgs flake input to be passed to nix/module.nix; use steampipe.nixosModules.default from the flake output.";
      }
    ];

    warnings =
      lib.optional (cfg.natInterface != null)
      "services.steampipe-cluster.natInterface is deprecated; NetworkManager shared mode manages NAT via the active default route."
      ++ lib.optional (cfg.tapOwner == null)
      "services.steampipe-cluster: tapOwner is not set; loginStateDir will be owned by root and the in-VM Steam user cannot write to it. Set tapOwner to a host user whose numeric UID matches the in-VM vm_user (default 1000).";

    # Bridge via NetworkManager
    networking.networkmanager.ensureProfiles.profiles =
      {
        "${cfg.bridge}" = mkBridgeProfile {
          name = cfg.bridge;
          address = cfg.hostIp;
          inherit (cfg) prefix;
          dhcpRange = "${cfg.subnet}.2,${cfg.subnet}.253";
        };
      }
      // tapProfiles;

    # Create login state directories for interactive and automated Steam login.
    systemd.tmpfiles.rules =
      ["d ${cfg.loginStateDir} 0755 ${loginStateOwner} users -"]
      ++ (map (name: "d ${cfg.loginStateDir}/${name} 0700 ${loginStateOwner} users -") vmNames)
      ++ lib.optionals cfg.loginRunners.enable [
        "d /var/lib/steampipe 0755 root root -"
        "d ${loginRunnerSshDir} 0700 ${loginStateOwner} users -"
        "d ${loginRunnerAuthorizedKeysDir} 0755 ${loginStateOwner} users -"
      ];

    system.activationScripts.steampipe-loginrunners-ssh-key = lib.mkIf cfg.loginRunners.enable {
      text = loginRunnerKeyScript;
    };

    # Write module config so project-level flakes can discover NixOS-level settings.
    # schemaVersion 2 keeps the JSON shape unchanged but changes loginStateDir
    # consumers from boot-time read-only injection to one writable Steam share.
    environment.etc."steampipe/module.json" = {
      text = builtins.toJSON ({
          accounts =
            lib.mapAttrsToList (name: creds: {
              steamUser = creds.steamUser;
              vm = name;
            })
            cfg.accounts;
          bridge = cfg.bridge;
          credentialsPath =
            if cfg.accounts != {}
            then "/etc/steampipe/credentials.toml"
            else null;
          hostIp = cfg.hostIp;
          loginStateDir = toString cfg.loginStateDir;
          prefix = cfg.prefix;
          schemaVersion = 2;
          subnet = cfg.subnet;
          tapOwner = cfg.tapOwner;
          vmCount = cfg.vmCount;
        }
        // lib.optionalAttrs cfg.loginRunners.enable {
          loginRunnersDir = "/etc/steampipe/login-runners";
          sshKey = loginRunnerSshKey;
        }
        // {
          inherit vmUser;
        });
    };

    environment.etc."steampipe/login-runners" = lib.mkIf cfg.loginRunners.enable {
      source = loginRunnersDir;
    };

    systemd.services.steampipe-loginrunners-ssh-key = lib.mkIf cfg.loginRunners.enable {
      description = "Generate steampipe login-runner SSH keypair";
      wantedBy = ["multi-user.target"];
      before = ["steampipe-gen-credentials.service"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        UMask = "0077";
      };
      script = loginRunnerKeyScript;
    };

    # Generate credentials TOML at runtime by reading agenix password files
    systemd.services.steampipe-gen-credentials = lib.mkIf (cfg.accounts != {}) {
      description = "Generate steampipe credentials TOML from secret files";
      after = ["agenix.service"];
      wantedBy = ["multi-user.target"];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStartPre = "${pkgs.coreutils}/bin/rm -f /etc/steampipe/credentials.toml";
        ExecStart = credentialsGenScript;
      };
    };

    # Trust the bridge interface in the NixOS firewall.
    # `trustedInterfaces` adds an `iifname "<bridge>" accept` rule near the top
    # of the nixos-fw input chain, but on some host configurations the rule
    # ordering or backend (iptables-nft vs pure nftables) can hide it from
    # textual checks. Add `extraInputRules` as a belt-and-braces explicit rule
    # so VM→host TCP works deterministically and the cluster preflight check
    # has a stable substring to match against.
    networking.firewall.trustedInterfaces = [cfg.bridge];
    networking.firewall.extraInputRules = ''
      iifname "${cfg.bridge}" accept comment "steampipe-cluster: trust VM bridge"
    '';
  };
}
