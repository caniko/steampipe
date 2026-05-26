{
  hypervisor,
  shareSteamState,
}: let
  supportsBalloon = builtins.elem hypervisor ["qemu" "cloud-hypervisor"];
  supportsInitialBalloon = hypervisor == "cloud-hypervisor";
in {
  inherit supportsBalloon supportsInitialBalloon;

  balloon = supportsBalloon;
  deflateOnOOM = supportsBalloon;
  initialBalloonMem =
    if supportsInitialBalloon
    then
      if shareSteamState
      then 1024
      else 512
    else 0;
}
