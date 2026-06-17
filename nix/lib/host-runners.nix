{
  pkgs,
  lib,
  nixpkgs,
  steampipe,
}: {
  vmCount,
  subnet ? "10.0.100",
  prefix ? 24,
  vmUser ? "cluster",
  sshAuthorizedKey ? "",
  hypervisor ? "crosvm",
  # Ignored for host-module classes. They always run without virtio-gpu and
  # use software rendering when a GUI is needed.
  graphics ? false,
  memory ? 2048,
  linkFarmName ? "steampipe-vm-runners",
  useSystemdStage1 ? true,
  loginStateDir ? null,
  extraVmModules ? [],
}: let
  network = {
    bridge = "br-cluster";
    inherit subnet prefix;
    hostIp = "${subnet}.254";
  };

  vmNames = map (i: "vm-${toString i}") (lib.range 1 vmCount);
  vms =
    lib.genAttrs
    vmNames
    (name: let
      index = lib.toInt (lib.removePrefix "vm-" name);
    in {
      ip = "${subnet}.${toString index}";
      mac = "52:54:00:cb:00:0${toString index}";
      inherit memory index;
      cores = 1;
    });

  flavor = {
    inherit hypervisor useSystemdStage1;
    graphics = false;
    memory = null;
    gpuProfile = "none";
  };

  mkMicroVM =
    import ./microvm-runner.nix {
      inherit pkgs lib nixpkgs steampipe;
    } {
      inherit network vmUser;
      # Empty key is impossible through the NixOS module: it asserts before
      # runners can be built.
      sshAuthorizedKeys = lib.optional (sshAuthorizedKey != "") sshAuthorizedKey;
      inherit loginStateDir;
      extraVmModules =
        extraVmModules
        ++ [
          {
            steampipe.vmGraphics.softwareRendering = true;
          }
        ];
    };

  vmRunners = lib.mapAttrs (mkMicroVM flavor) vms;
in
  pkgs.linkFarm linkFarmName
  (lib.mapAttrsToList (name: runner: {
      inherit name;
      path = runner;
    })
    vmRunners)
