{
  microvm,
  nixpkgs,
  self,
}: {
  nixosModules =
    microvm.nixosModules
    // {
      # Host networking (bridge, TAPs, NAT)
      default = {...}: {
        imports = [
          microvm.nixosModules.host
          ../module.nix
        ];
        _module.args = {
          inherit nixpkgs microvm;
          steampipe = self;
        };
      };
      # VM-side base config for reusable steampipe test clusters.
      cluster-vm-base = import ../cluster-vm-base.nix;
      # VM-side graphics setup (mesa, virtio-gpu kernel modules, overlay)
      vm-graphics = import ../vm-graphics.nix;
      # Host-level timer for refreshing Steam sessions.
      warmTimer = import ../modules/warm-timer.nix;
      # Host-level preset: headless runtime runners plus weekly Steam refresh.
      steamSessionRefresh = import ../modules/steam-session-refresh.nix;
    };

  lib = {
    mkTestCluster = import ../lib/test-cluster.nix;
  };
}
