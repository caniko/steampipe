{
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.steampipe-cluster;
  vmNames = builtins.genList (i: "vm-${toString (i + 1)}") cfg.vmCount;
  tapNames = map (n: "tap-${n}") vmNames;

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
        the Steam config files. Project-level flakes mount these
        read-only into VMs to reuse login sessions.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    warnings =
      lib.optional (cfg.natInterface != null)
      "services.steampipe-cluster.natInterface is deprecated; NetworkManager shared mode manages NAT via the active default route.";

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
      ["d ${cfg.loginStateDir} 0755 ${cfg.tapOwner or "root"} users -"]
      ++ (map (name: "d ${cfg.loginStateDir}/${name} 0755 ${cfg.tapOwner or "root"} users -") vmNames);

    # Write module config so project-level flakes can discover NixOS-level settings
    environment.etc."steampipe/module.json" = {
      text = builtins.toJSON {
        accounts = lib.mapAttrsToList (name: creds: {
          steamUser = creds.steamUser;
          vm = name;
        }) cfg.accounts;
        bridge = cfg.bridge;
        credentialsPath =
          if cfg.accounts != {}
          then "/etc/steampipe/credentials.toml"
          else null;
        hostIp = cfg.hostIp;
        loginStateDir = toString cfg.loginStateDir;
        prefix = cfg.prefix;
        schemaVersion = 1;
        subnet = cfg.subnet;
        tapOwner = cfg.tapOwner;
        vmCount = cfg.vmCount;
      };
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
