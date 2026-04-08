{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.steampipe-cluster;
  vmNames = builtins.genList (i: "vm-${toString (i + 1)}") cfg.vmCount;
  tapNames = map (n: "tap-${n}") vmNames;

  tapSetupScript = pkgs.writeShellScript "steampipe-tap-setup" ''
    set -euo pipefail

    # Wait for NetworkManager to create the bridge (profile activation is async)
    for i in $(seq 1 30); do
      if ip link show "${cfg.bridge}" &>/dev/null; then
        break
      fi
      echo "Waiting for ${cfg.bridge}... ($i/30)"
      sleep 1
    done
    if ! ip link show "${cfg.bridge}" &>/dev/null; then
      echo "ERROR: ${cfg.bridge} did not appear after 30s" >&2
      exit 1
    fi

    ${lib.concatMapStringsSep "\n" (tap: ''
      if ! ip link show "${tap}" &>/dev/null; then
        ip tuntap add "${tap}" mode tap ${lib.optionalString (cfg.tapOwner != null) "user ${cfg.tapOwner}"} vnet_hdr
      fi
      ip link set "${tap}" master "${cfg.bridge}"
      ip link set "${tap}" up
    '') tapNames}
  '';
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

    # Bridge via NetworkManager
    networking.networkmanager.ensureProfiles.profiles."${cfg.bridge}" = {
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

    # TAP devices via systemd oneshot (NM can't create TAP interfaces)
    systemd.services.steampipe-tap-setup = {
      description = "Create TAP devices for steampipe cluster VMs";
      after = ["NetworkManager.service" "NetworkManager-wait-online.service"];
      wants = ["NetworkManager.service"];
      wantedBy = ["multi-user.target"];
      path = [pkgs.iproute2];
      serviceConfig = {
        Type = "oneshot";
        RemainAfterExit = true;
        ExecStart = tapSetupScript;
      };
    };

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
