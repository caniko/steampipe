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
  memoryBudget = import ./microvm-memory-budget.nix {
    inherit shareSteamState;
    inherit (flavor) hypervisor;
  };
  memoryMiB =
    if flavor.memory != null
    then flavor.memory
    else vm.memory;
in
  (nixpkgs.lib.nixosSystem {
    system = "x86_64-linux";
    modules =
      [
        steampipe.nixosModules.microvm
        steampipe.nixosModules.vm-graphics
        steampipe.nixosModules.cluster-vm-base
        ({
          config,
          pkgs,
          ...
        }: {
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
          environment.sessionVariables = lib.mkIf (flavor.graphics && flavor.gpuProfile != "egl") {
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
            # microvm's optimize step rebuilds qemu_kvm with nixosTestRunner,
            # a variant Hydra never builds. qemu_test already has it, so the
            # override is a no-op and the runner substitutes from
            # cache.nixos.org. Graphical guests keep the GUI-capable default.
            qemu.package =
              lib.mkIf (flavor.hypervisor == "qemu" && !flavor.graphics)
              config.microvm.vmHostPackages.qemu_test;
            cloud-hypervisor.package =
              lib.mkIf (flavor.hypervisor == "cloud-hypervisor" && !flavor.graphics)
              pkgs.cloud-hypervisor;
            # crosvm's multi-process device proxy uses minijail+seccomp.
            # We point crosvm at the seccomp policies installed by our
            # patched crosvm package; the pre-compiled `.bpf` form is what
            # actually gets loaded.
            crosvm.extraArgs = lib.mkIf (flavor.hypervisor == "crosvm") [
              "--seccomp-policy-dir=${pkgs.crosvm}/share/policy/${pkgs.stdenv.hostPlatform.linuxArch}"
              "--no-usb"
            ];
            mem = memoryMiB;
            balloon = lib.mkIf memoryBudget.balloon true;
            deflateOnOOM = lib.mkIf memoryBudget.deflateOnOOM true;
            # The pinned microvm.nix QEMU runner rejects non-zero
            # initialBalloonMem; cloud-hypervisor supports it.
            initialBalloonMem =
              lib.mkIf memoryBudget.supportsInitialBalloon
              memoryBudget.initialBalloonMem;
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
                # Login runners mount Steam state from `loginStateDir`; their
                # home image holds only small residual config and login state.
                size =
                  if shareSteamState
                  then 2048
                  else 8192;
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
