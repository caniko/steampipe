{
  config,
  lib,
  ...
}:

let
  cfg = config.services.steampipe-cluster;
  vmNames = builtins.genList (i: "vm-${toString (i + 1)}") cfg.vmCount;
  tapNames = map (n: "tap-${n}") vmNames;
  cidr = "${cfg.subnet}.0/${toString cfg.prefix}";

  tapNetdevs = builtins.listToAttrs (map (tap: {
    name = "40-${tap}";
    value.netdevConfig = {
      Name = tap;
      Kind = "tap";
    };
  }) tapNames);

  tapNetworks = builtins.listToAttrs (map (tap: {
    name = "40-${tap}";
    value = {
      matchConfig.Name = tap;
      networkConfig.Bridge = cfg.bridge;
    };
  }) tapNames);
in
{
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
        Outbound interface for NAT masquerade (e.g. "enp5s0").
        If null, masquerade applies to all outbound traffic.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    boot.kernel.sysctl."net.ipv4.ip_forward" = lib.mkDefault 1;

    systemd.network.enable = true;

    # Bridge + TAP netdevs
    systemd.network.netdevs = {
      "40-${cfg.bridge}".netdevConfig = {
        Name = cfg.bridge;
        Kind = "bridge";
      };
    } // tapNetdevs;

    # Bridge IP + TAP→bridge attachment
    systemd.network.networks = {
      "40-${cfg.bridge}" = {
        matchConfig.Name = cfg.bridge;
        address = ["${cfg.hostIp}/${toString cfg.prefix}"];
        networkConfig.ConfigureWithoutCarrier = true;
      };
    } // tapNetworks;

    # Trust the bridge interface in the NixOS firewall
    networking.firewall.trustedInterfaces = [cfg.bridge];

    # nftables: NAT + forwarding
    networking.nftables.enable = true;
    networking.nftables.tables.cluster = {
      family = "ip";
      content = ''
        chain nat_post {
          type nat hook postrouting priority 100 ;
          ip saddr ${cidr} ${
            if cfg.natInterface != null
            then ''oifname "${cfg.natInterface}"''
            else ""
          } masquerade
        }

        chain forward {
          type filter hook forward priority 0 ;
          iifname "${cfg.bridge}" accept
          oifname "${cfg.bridge}" accept
        }
      '';
    };
  };
}
