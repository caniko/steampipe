# services.steampipe Steam session refresh preset.
#
# This composes the host cluster module with the warm timer defaults needed to
# keep Steam refresh tokens active. Consumer-specific accounts, secrets, and SSH
# public keys stay in the consuming host configuration.
{
  config,
  lib,
  ...
}: {
  imports = [
    ./warm-timer.nix
  ];

  config = {
    services.steampipe-cluster.runners = {
      enable = lib.mkDefault true;
      sshAuthorizedKey = lib.mkDefault config.services.steampipe-cluster.loginRunners.sshAuthorizedKey;
    };

    services.steampipe-warm-timer = {
      enable = lib.mkDefault true;
      onCalendar = lib.mkDefault "weekly";
      randomizedDelaySec = lib.mkDefault "1h";
    };
  };
}
