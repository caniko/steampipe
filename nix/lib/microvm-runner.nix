{
  pkgs,
  lib,
  nixpkgs,
  steampipe,
}: {
  network,
  vmUser ? "cluster",
  sshAuthorizedKeys ? [],
  loginStateDir ? null,
  extraVmOverlays ? [],
  extraVmModules ? [],
}: flavor: name: vm: let
  shareSteamState = loginStateDir != null;
in
  (nixpkgs.lib.nixosSystem {
    system = "x86_64-linux";
    modules =
      [
        steampipe.nixosModules.microvm
        steampipe.nixosModules.vm-graphics
        steampipe.nixosModules.cluster-vm-base
        ({pkgs, ...}: {
          nixpkgs.overlays =
            [steampipe.overlays.default]
            ++ extraVmOverlays;

          steampipe.clusterVm = {
            user = vmUser;
            inherit sshAuthorizedKeys;
            enableSteam = true;
            useSystemdStage1 = flavor.useSystemdStage1;
          };

          networking.hostName = name;
          environment.sessionVariables = lib.mkIf (flavor.gpuProfile != "egl") {
            VK_DRIVER_FILES = "/run/opengl-driver/share/vulkan/icd.d/virtio_icd.x86_64.json";
          };
          networking.interfaces.eth0.ipv4.addresses = [
            {
              address = vm.ip;
              prefixLength = network.prefix;
            }
          ];

          microvm = {
            hypervisor = flavor.hypervisor;
            graphics.enable = flavor.graphics;
            # crosvm's multi-process device proxy uses minijail+seccomp.
            # We point crosvm at the seccomp policies installed by our
            # patched crosvm package; the pre-compiled `.bpf` form is what
            # actually gets loaded.
            crosvm.extraArgs = lib.mkIf (flavor.hypervisor == "crosvm") [
              "--seccomp-policy-dir=${pkgs.crosvm}/share/policy/${pkgs.stdenv.hostPlatform.linuxArch}"
              "--no-usb"
            ];
            mem =
              if flavor.memory != null
              then flavor.memory
              else vm.memory;
            vcpu = vm.cores;
            vsock.cid = vm.index + 3; # CIDs 0-2 are reserved

            # TAP interface (pre-created by NixOS module, attached to br-cluster).
            interfaces = [
              {
                type = "tap";
                id = "tap-${name}";
                mac = vm.mac;
              }
            ];

            # Home volume stays: the disk image holds non-Steam home state.
            volumes = [
              {
                mountPoint = "/home/${vmUser}";
                image = "${name}-home.img";
                size = 8192;
              }
            ];

            shares = lib.optionals shareSteamState [
              {
                tag = "steam-state-${name}";
                source = "${loginStateDir}/${name}";
                mountPoint = "/home/${vmUser}/.local/share/Steam";
                proto = "virtiofs";
              }
            ];
          };
        })
      ]
      ++ extraVmModules;
  })
  .config
  .microvm
  .declaredRunner
