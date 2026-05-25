{
  pkgs,
  lib,
  nixpkgs,
  steampipe,
}:
args:
(import ./host-runners.nix {
  inherit pkgs lib nixpkgs steampipe;
})
(args
  // {
    memory = args.memory or 4096;
    linkFarmName = "steampipe-login-vm-runners";
  })
