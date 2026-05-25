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
  graphics ? true,
  memory ? 4096,
  useSystemdStage1 ? false,
}: let
  network = {
    bridge = "br-cluster";
    inherit subnet prefix;
    hostIp = "${subnet}.254";
  };

  vmNames = map (i: "vm-${toString i}") (lib.range 1 vmCount);
  loginVMs =
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
    inherit hypervisor graphics useSystemdStage1;
    memory = null;
    gpuProfile = "vulkan";
  };

  mkMicroVM = import ./microvm-runner.nix {
    inherit pkgs lib nixpkgs steampipe;
  } {
    inherit network vmUser;
    # Empty key produces empty authorized_keys; runners build but are
    # unreachable. This is the intentional first-rebuild state: the host
    # activation script generates the keypair, the next rebuild's eval
    # reads it via the module's default, and the runners gain a usable key.
    sshAuthorizedKeys = lib.optional (sshAuthorizedKey != "") sshAuthorizedKey;
    loginStateDir = null;
  };

  loginVMRunners = lib.mapAttrs (mkMicroVM flavor) loginVMs;
in
  pkgs.linkFarm "steampipe-login-vm-runners"
    (lib.mapAttrsToList (name: runner: {
        inherit name;
        path = runner;
      })
      loginVMRunners)
