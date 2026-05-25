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
  sshAuthorizedKey ? null,
  sshAuthorizedKeysPath ? null,
  sshAuthorizedKeysShare ? null,
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
    inherit network vmUser sshAuthorizedKeysPath sshAuthorizedKeysShare;
    sshAuthorizedKeys = lib.optional (sshAuthorizedKey != null) sshAuthorizedKey;
    loginStateDir = null;
  };

  loginVMRunners = lib.mapAttrs (mkMicroVM flavor) loginVMs;
in
  assert lib.assertMsg (sshAuthorizedKey != null || sshAuthorizedKeysPath != null)
  "login-runners.nix requires either sshAuthorizedKey or sshAuthorizedKeysPath";
    pkgs.linkFarm "steampipe-login-vm-runners"
    (lib.mapAttrsToList (name: runner: {
        inherit name;
        path = runner;
      })
      loginVMRunners)
