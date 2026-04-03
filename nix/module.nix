{
  config,
  lib,
  ...
}:

let
  cfg = config.services.steampipe-cluster;
  vmNames = builtins.genList (i: "vm-${toString (i + 1)}") cfg.vmCount;
  tapNames = map (n: "tap-${n}") vmNames;

  # NM ensureProfiles for TAP devices (bridge slaves)
  tapProfiles = builtins.listToAttrs (map (tap: {
    name = tap;
    value = {
      connection = {
        id = tap;
        type = "tun";
        master = cfg.bridge;
        slave-type = "bridge";
        autoconnect = "true";
      };
      tun = {
        mode = "2"; # TAP
        vnet-hdr = "true";
      } // lib.optionalAttrs (cfg.tapOwner != null) {
        owner = cfg.tapOwner;
      };
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

    tapOwner = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      description = ''
        User who owns the TAP devices. Required for unprivileged VM
        hypervisors (e.g. cloud-hypervisor) to open TAP devices.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    boot.kernel.sysctl."net.ipv4.ip_forward" = lib.mkDefault 1;

    # Bridge + TAP devices via NetworkManager
    networking.networkmanager.ensureProfiles.profiles =
      {
        "${cfg.bridge}" = {
          connection = {
            id = cfg.bridge;
            type = "bridge";
            interface-name = cfg.bridge;
            autoconnect = "true";
          };
          bridge.stp = "false";
          ipv4 = {
            method = "manual";
            addresses = "${cfg.hostIp}/${toString cfg.prefix}";
          };
          ipv6.method = "disabled";
        };
      }
      // tapProfiles;

    # Trust the bridge interface in the NixOS firewall
    networking.firewall.trustedInterfaces = [cfg.bridge];

    # NAT masquerade via NixOS module (auto-uses nftables when enabled)
    networking.nat = {
      enable = true;
      externalInterface = cfg.natInterface;
      internalInterfaces = [cfg.bridge];
    };
  };
}
