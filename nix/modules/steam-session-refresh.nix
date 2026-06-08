# services.steampipe Steam session refresh preset.
#
# This composes the host cluster module with the timer defaults needed to keep
# Steam refresh tokens active. Consumer-specific accounts and secrets stay in
# the consuming host configuration.
{
  config,
  lib,
  ...
}: {
  imports = [
    ./warm-timer.nix
  ];

  config = {
    services.steampipe-warm-timer = {
      enable = lib.mkDefault true;
      onCalendar = lib.mkDefault "weekly";
      randomizedDelaySec = lib.mkDefault "1h";
    };
  };
}
