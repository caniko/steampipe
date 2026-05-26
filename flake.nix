{
  description = "NixOS microVM cluster orchestrator";

  nixConfig = {
    extra-substituters = ["https://microvm.cachix.org"];
    extra-trusted-public-keys = ["microvm.cachix.org-1:oXnBc6hRE3eX5rSYdRyMYXnfzcCxC7yKPTbZXALsqys="];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
    microvm.url = "github:astro/microvm.nix";
  };

  outputs = {
    self,
    nixpkgs,
    rust-overlay,
    flake-utils,
    microvm,
    ...
  }:
    (flake-utils.lib.eachDefaultSystem (system: let
      pkgs = import nixpkgs {
        inherit system;
        overlays = [(import rust-overlay)];
      };
      inherit (pkgs) lib;

      steampipeModule = {...}: {
        imports = [
          microvm.nixosModules.host
          ./nix/module.nix
        ];
        _module.args = {
          inherit nixpkgs microvm;
          steampipe = self;
        };
      };

      toolchain = pkgs.rust-bin.nightly.latest.minimal.override {
        extensions = [
          "clippy"
          "rustfmt"
        ];
      };

      docs = pkgs.stdenv.mkDerivation {
        pname = "steampipe-docs";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.unions [
            ./docs/book.toml
            ./docs/src/SUMMARY.md
            ./docs/src/introduction.md
            ./docs/src/getting-started/installation.md
            ./docs/src/getting-started/quick-start.md
            ./docs/src/configuration/steampipe-toml.md
            ./docs/src/configuration/steam-credentials.md
            ./docs/src/configuration/vm-lifecycle.md
            ./docs/src/testing/e2e-tests.md
            ./docs/src/testing/history.md
            ./docs/src/networking/cluster-network.md
            ./docs/src/networking/network-simulation.md
          ];
        };
        nativeBuildInputs = [pkgs.mdbook];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src docs
          cd docs/docs
          mdbook build
        '';
        installPhase = ''
          cp -r book $out
        '';
      };

      website = pkgs.stdenv.mkDerivation {
        pname = "steampipe-website";
        version = "0.1.0";
        src = lib.fileset.toSource {
          root = ./.;
          fileset = lib.fileset.maybeMissing ./website;
        };
        nativeBuildInputs = [pkgs.zola];
        phases = ["buildPhase" "installPhase"];
        buildPhase = ''
          cp -r --no-preserve=mode $src/website site
          cd site
          zola build
        '';
        installPhase = ''
          cp -r public $out
        '';
      };

      site = pkgs.runCommand "steampipe-site" {} ''
        mkdir -p $out
        cp -r ${website}/* $out/
        mkdir -p $out/docs
        cp -r ${docs}/* $out/docs/
      '';

      cluster-ctl = pkgs.rustPlatform.buildRustPackage {
        pname = "cluster-ctl";
        version = "0.1.0";
        src = ./.;
        cargoHash = "sha256-3QhzDItuizh7xP5qMgEi+0e2Qaad6C9M7zRHm6RfGio=";
        nativeBuildInputs = [toolchain pkgs.makeWrapper];
        postInstall = ''
          wrapProgram $out/bin/cluster-ctl \
            --prefix PATH : ${lib.makeBinPath [pkgs.weston]}
        '';
      };

      warmTimerEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          (import ./nix/lib/warm-timer.nix {
            clusterCtl = pkgs.writeShellScriptBin "cluster-ctl" "exit 0";
            vmRunnersDir = pkgs.runCommand "warm-timer-runners" {} "mkdir -p $out";
            vmCount = 7;
            defaultUser = "cluster";
            defaultSshKey = "/home/cluster/.ssh/cluster_key";
          })
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-warm-timer.enable = true;
          }
        ];
      };

      warmTimerDisabledEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          (import ./nix/lib/warm-timer.nix {
            clusterCtl = pkgs.writeShellScriptBin "cluster-ctl" "exit 0";
            vmRunnersDir = pkgs.runCommand "warm-timer-runners-disabled" {} "mkdir -p $out";
            vmCount = 7;
            defaultUser = "cluster";
            defaultSshKey = "/home/cluster/.ssh/cluster_key";
          })
          {
            system.stateVersion = "26.05";
          }
        ];
      };

      hostWarmTimerEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          ./nix/modules/warm-timer.nix
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
              runners = {
                enable = true;
                sshAuthorizedKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForSteampipeEvalChecksOnly steampipe-test";
              };
            };
            services.steampipe-warm-timer.enable = true;
          }
        ];
      };

      hostWarmTimerMissingRunnersEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          ./nix/modules/warm-timer.nix
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
            };
            services.steampipe-warm-timer.enable = true;
          }
        ];
      };

      moduleLoginRunnersEnabledEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
              loginRunners = {
                enable = true;
                sshAuthorizedKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForSteampipeEvalChecksOnly steampipe-test";
              };
            };
          }
        ];
      };

      moduleLoginRunnersEmptyKeyEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
              loginRunners.enable = true;
            };
          }
        ];
      };

      moduleLoginRunnersDisabledEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
            };
          }
        ];
      };

      moduleRunnersEmptyKeyEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
              runners.enable = true;
            };
          }
        ];
      };

      moduleRunnersEnabledEval = nixpkgs.lib.nixosSystem {
        inherit system;
        modules = [
          steampipeModule
          {
            system.stateVersion = "26.05";
            users.users.cluster = {
              isNormalUser = true;
              home = "/home/cluster";
            };
            services.steampipe-cluster = {
              enable = true;
              vmCount = 2;
              tapOwner = "cluster";
              tapOwnerUid = 1000;
              runners = {
                enable = true;
                sshAuthorizedKey = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFakeKeyForSteampipeEvalChecksOnly steampipe-test";
              };
            };
          }
        ];
      };
    in {
      packages = {
        default = cluster-ctl;
        inherit docs website site;
      };

      checks = lib.optionalAttrs pkgs.stdenv.isLinux {
        warm-timer-eval =
          pkgs.runCommand "warm-timer-eval" {
            enabled =
              if warmTimerEval.config.services.steampipe-warm-timer.enable
              then "true"
              else "false";
            disabledByDefault =
              if warmTimerDisabledEval.config.services.steampipe-warm-timer.enable
              then "false"
              else "true";
            serviceUser = warmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.User;
            timerOnCalendar = warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.OnCalendar;
            timerPersistent =
              if warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.Persistent
              then "true"
              else "false";
            timerRandomizedDelaySec = warmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.RandomizedDelaySec;
          } ''
            test "$enabled" = true
            test "$disabledByDefault" = true
            test "$serviceUser" = cluster
            test "$timerOnCalendar" = weekly
            test "$timerPersistent" = true
            test "$timerRandomizedDelaySec" = 1h
            touch "$out"
          '';

        host-warm-timer-eval =
          pkgs.runCommand "host-warm-timer-eval" {
            timerOnCalendar = hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.OnCalendar;
            timerPersistent =
              if hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.Persistent
              then "true"
              else "false";
            timerRandomizedDelaySec = hostWarmTimerEval.config.systemd.timers.steampipe-steam-warm.timerConfig.RandomizedDelaySec;
            serviceUser = hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.User;
            serviceHome = builtins.elemAt hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.Environment 0;
            warmCommand = hostWarmTimerEval.config.systemd.services.steampipe-steam-warm.serviceConfig.ExecStart;
          } ''
            test "$timerOnCalendar" = weekly
            test "$timerPersistent" = true
            test "$timerRandomizedDelaySec" = 1h
            test "$serviceUser" = cluster
            test "$serviceHome" = HOME=/home/cluster
            warmCommandText="$(cat "$warmCommand")"
            case "$warmCommandText" in
              *"--runners-dir"*) echo "host warm timer should rely on module.json, not --runners-dir" >&2; exit 1 ;;
            esac
            case "$warmCommandText" in
              *"cluster-ctl"*"--vm-count 2"*"--ssh-key /var/lib/steampipe/ssh/cluster_key"*"steam warm"*) ;;
              *) echo "unexpected warm command: $warmCommandText" >&2; exit 1 ;;
            esac
            touch "$out"
          '';

        host-warm-timer-missing-runners-eval =
          pkgs.runCommand "host-warm-timer-missing-runners-eval" {
            assertionMessage =
              let
                failed =
                  lib.findFirst
                  (assertion: !assertion.assertion)
                  null
                  hostWarmTimerMissingRunnersEval.config.assertions;
              in
                if failed == null
                then ""
                else failed.message;
          } ''
            case "$assertionMessage" in
              *"services.steampipe-warm-timer.enable requires services.steampipe-cluster.runners.enable"*) ;;
              *) echo "expected runners.enable assertion not seen: $assertionMessage" >&2; exit 1 ;;
            esac
            touch "$out"
          '';

        module-loginrunners-eval =
          pkgs.runCommand "module-loginrunners-eval" {
            enabledJson = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/module.json".text;
            disabledJson = moduleLoginRunnersDisabledEval.config.environment.etc."steampipe/module.json".text;
            loginRunnersDir = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/login-runners".source;
          } ''
            ${pkgs.python3}/bin/python - <<'PY'
import json
import os

enabled = json.loads(os.environ["enabledJson"])
disabled = json.loads(os.environ["disabledJson"])

assert enabled["schemaVersion"] == 2
assert enabled["vmCount"] == 2
assert enabled["vmUser"] == "cluster"
assert enabled["loginRunnersDir"] == "/etc/steampipe/login-runners"
assert enabled["sshKey"] == "/var/lib/steampipe/ssh/cluster_key"

assert disabled["schemaVersion"] == 2
assert disabled["vmCount"] == 2
assert disabled["vmUser"] == "cluster"
assert "loginRunnersDir" not in disabled
assert "sshKey" not in disabled
PY

            test -x "$loginRunnersDir/vm-1/bin/microvm-run"
            test -x "$loginRunnersDir/vm-2/bin/microvm-run"
            test ! -e "$loginRunnersDir/vm-3"
            touch "$out"
          '';

        module-runners-eval =
          pkgs.runCommand "module-runners-eval" {
            enabledJson = moduleRunnersEnabledEval.config.environment.etc."steampipe/module.json".text;
            disabledJson = moduleLoginRunnersDisabledEval.config.environment.etc."steampipe/module.json".text;
            runnersDir = moduleRunnersEnabledEval.config.environment.etc."steampipe/runners".source;
            runnersEtcEnabled =
              if moduleRunnersEnabledEval.config.environment.etc ? "steampipe/runners"
              then "true"
              else "false";
            runnersEtcDisabled =
              if moduleLoginRunnersDisabledEval.config.environment.etc ? "steampipe/runners"
              then "true"
              else "false";
          } ''
            ${pkgs.python3}/bin/python - <<'PY'
import json
import os

enabled = json.loads(os.environ["enabledJson"])
disabled = json.loads(os.environ["disabledJson"])

assert enabled["schemaVersion"] == 2
assert enabled["vmCount"] == 2
assert enabled["vmUser"] == "cluster"
assert enabled["runnersDir"] == "/etc/steampipe/runners"
assert enabled["sshKey"] == "/var/lib/steampipe/ssh/cluster_key"
assert "loginRunnersDir" not in enabled

assert disabled["schemaVersion"] == 2
assert disabled["vmCount"] == 2
assert disabled["vmUser"] == "cluster"
assert "runnersDir" not in disabled
assert "sshKey" not in disabled
PY

            test "$runnersEtcEnabled" = true
            test "$runnersEtcDisabled" = false
            test -x "$runnersDir/vm-1/bin/microvm-run"
            test -x "$runnersDir/vm-2/bin/microvm-run"
            test ! -e "$runnersDir/vm-3"
            touch "$out"
          '';

        # Regression guard: loginRunners must not silently build VMs with an
        # empty sshAuthorizedKey default under pure flake evaluation.
        module-loginrunners-empty-key-asserts =
          pkgs.runCommand "module-loginrunners-empty-key-asserts" {
            assertionMessage =
              let
                failed =
                  lib.findFirst
                  (assertion: !assertion.assertion)
                  null
                  moduleLoginRunnersEmptyKeyEval.config.assertions;
              in
                if failed == null
                then ""
                else failed.message;
          } ''
            case "$assertionMessage" in
              *"loginRunners.enable is true but"*"sshAuthorizedKey is empty"*) ;;
              *) echo "expected loginRunners empty sshAuthorizedKey assertion not seen: $assertionMessage" >&2; exit 1 ;;
            esac
            touch "$out"
          '';

        # Regression guard: legacy runners must not silently build VMs with an
        # empty sshAuthorizedKey default under pure flake evaluation.
        module-runners-empty-key-asserts =
          pkgs.runCommand "module-runners-empty-key-asserts" {
            assertionMessage =
              let
                failed =
                  lib.findFirst
                  (assertion: !assertion.assertion)
                  null
                  moduleRunnersEmptyKeyEval.config.assertions;
              in
                if failed == null
                then ""
                else failed.message;
          } ''
            case "$assertionMessage" in
              *"runners.enable is true but"*"sshAuthorizedKey is empty"*) ;;
              *) echo "expected runners empty sshAuthorizedKey assertion not seen: $assertionMessage" >&2; exit 1 ;;
            esac
            touch "$out"
          '';

        # Regression guard: the empty-key assertion must not reject a configured
        # loginRunners.sshAuthorizedKey on the normal happy path.
        module-loginrunners-with-key-evals =
          pkgs.runCommand "module-loginrunners-with-key-evals" {
            enabled =
              if moduleLoginRunnersEnabledEval.config.services.steampipe-cluster.loginRunners.enable
              then "true"
              else "false";
            loginRunnersDir = moduleLoginRunnersEnabledEval.config.environment.etc."steampipe/login-runners".source;
          } ''
            test "$enabled" = true
            test -x "$loginRunnersDir/vm-1/bin/microvm-run"
            touch "$out"
          '';

        module-cluster-ctl-install-eval =
          pkgs.runCommand "module-cluster-ctl-install-eval" {
            packagesText = builtins.concatStringsSep " " (map (p: p.pname or "") moduleLoginRunnersEnabledEval.config.environment.systemPackages);
          } ''
            case "$packagesText" in
              *cluster-ctl*) ;;
              *) echo "cluster-ctl not in environment.systemPackages: $packagesText" >&2; exit 1 ;;
            esac
            touch "$out"
          '';

        module-overlay-applied-eval =
          pkgs.runCommand "module-overlay-applied-eval" {
            crosvmMarker =
              if (moduleLoginRunnersEnabledEval.pkgs.crosvm.passthru.__steampipeOverlay or false)
              then "yes"
              else "no";
            cloudHypervisorMarker =
              if (moduleLoginRunnersEnabledEval.pkgs.cloud-hypervisor-graphics.passthru.__steampipeOverlay or false)
              then "yes"
              else "no";
            overlaysCount = toString (builtins.length moduleLoginRunnersEnabledEval.config.nixpkgs.overlays);
          } ''
            test "$crosvmMarker" = yes
            test "$cloudHypervisorMarker" = yes
            test "$overlaysCount" -gt 0
            touch "$out"
          '';
      };

      devShells.default = pkgs.mkShell {
        buildInputs = [
          toolchain
          pkgs.cargo-outdated
        ];
        packages = [
          pkgs.just
          pkgs.mdbook
          pkgs.zola
          cluster-ctl
          (pkgs.python3.withPackages (ps: [ps.pillow]))
        ];
        shellHook = ''
          echo "Website: cd website && zola serve"
          echo "Documentation: cd docs && mdbook serve"
        '';
      };
    }))
    // {
      nixosModules = microvm.nixosModules // {
        # Host networking (bridge, TAPs, NAT)
        default = {...}: {
          imports = [
            microvm.nixosModules.host
            ./nix/module.nix
          ];
          _module.args = {
            inherit nixpkgs microvm;
            steampipe = self;
          };
        };
        # VM-side base config for reusable steampipe test clusters.
        cluster-vm-base = import ./nix/cluster-vm-base.nix;
        # VM-side graphics setup (mesa, virtio-gpu kernel modules, overlay)
        vm-graphics = import ./nix/vm-graphics.nix;
        # Host-level timer for refreshing Steam sessions.
        warmTimer = import ./nix/modules/warm-timer.nix;
      };

      lib = {
        mkTestCluster = import ./nix/lib/test-cluster.nix;
      };

      # Overlay providing cloud-hypervisor-graphics (from microvm.nix +
      # spectrum-os patches) plus a hash override so it stays buildable when
      # nixpkgs' rustPlatform vendoring drifts ahead of spectrum's pinned hash.
      #
      # Background: microvm.nix's overlay defines `cloud-hypervisor-graphics`
      # by importing spectrum-os's `pkgs/cloud-hypervisor`. Spectrum pins a
      # specific cargoDeps hash for the patched build; when downstream consumers
      # follow nixos-unstable ahead of spectrum's pin, the hash goes stale and
      # `cloud-hypervisor-graphics` fails to build with
      # `Cargo.lock is not the same in /build/cargo-deps-vendor`.
      #
      # We pin the corrected hash here so steampipe consumers don't have to
      # redo this dance. Drop this override when the upstream spectrum pin
      # catches up: <https://spectrum-os.org/git/spectrum> pkgs/cloud-hypervisor
      overlays.default = nixpkgs.lib.composeExtensions microvm.overlay (final: prev: {
        cloud-hypervisor-graphics = prev.cloud-hypervisor-graphics.overrideAttrs (oldAttrs: {
          cargoDeps = final.rustPlatform.fetchCargoVendor {
            inherit (oldAttrs) src patches;
            hash = "sha256-YYrzd44DbaRT3jieclHrGoDLT14vTzTtNwIRLCtv3Ko=";
          };
          passthru = (oldAttrs.passthru or {}) // {__steampipeOverlay = true;};
        });

        # The nixpkgs crosvm derivation only installs the binary; the source
        # ships seccomp policies in `jail/seccomp/<arch>/`, and crosvm only
        # uses external policies when the operator passes `--seccomp-policy-dir`
        # (typical for diagnosing what `--seccomp-log-failures` is killing on
        # newer kernels than the embedded BPFs were compiled against).
        #
        # Install BOTH the source `.policy` files (rewritten to use a relative
        # `@include` base) AND the precompiled `.bpf` files (built with the same
        # `compile_seccomp_policy.py` crosvm itself uses, with default-action
        # `trap` to match the embedded build). minijail's runtime
        # `parse_seccomp_filters` text parser does not allow syscall
        # redefinition across `@include`s, so the `.bpf` form is what's
        # actually used unless `--seccomp-log-failures` forces text mode.
        crosvm = prev.crosvm.overrideAttrs (oldAttrs: {
          # microvm.nix's crosvm runner adds `--seccomp-log-failures` for any
          # crosvm older than 107.1 (a "workaround" tagged at the line in
          # `lib/runners/crosvm.nix`). Our nixpkgs crosvm reports its version
          # as `0-unstable-<date>` because upstream doesn't tag releases, so
          # the comparison flags it as old. `--seccomp-log-failures` forces
          # text-mode policy parsing, which crashes on the redefinitions
          # baked into crosvm's `@include` chain. Pretend to be 107.1 so the
          # microvm runner skips that workaround and uses our pre-compiled
          # `.bpf` policies instead.
          version = "107.1-unstable-2026-02-13";
          __intentionallyOverridingVersion = true;
          passthru = (oldAttrs.passthru or {}) // {__steampipeOverlay = true;};
          buildInputs = (oldAttrs.buildInputs or []) ++ [
            final.aemu
            final.gfxstream
          ];
          buildFeatures = (oldAttrs.buildFeatures or []) ++ [
            "gfxstream"
          ];
          nativeBuildInputs = (oldAttrs.nativeBuildInputs or []) ++ [
            final.python3
          ];

          # Patch the bundled `.policy` source files before crosvm's build
          # runs `compile_seccomp_policy.py` and bakes the resulting `.bpf`
          # programs into the binary (since crosvm 107.1, fb60a5c9473b).
          # Mirrors NixOS/nixpkgs#517363 — kept here verbatim so this overlay
          # is self-sufficient until that PR lands; the `postInstall` below
          # then re-uses the already-patched source policies.
          #
          # Two narrow widenings to `common_device.policy`, both required
          # for any multi-process Rust device proxy to function on
          # Linux ≥ 6.13 with glibc ≥ 2.40:
          #
          #   - prctl: arg0 == PR_SET_VMA  →  also allow PR_SET_NAME, which
          #     Rust's std::thread::Thread::new emits from the new thread
          #     before user code runs. Without it every device proxy worker
          #     thread dies with SIGSYS during pthread_create.
          #   - madvise: arg2 == <fixed MADV_* list>  →  also allow
          #     arg2 == 102 (MADV_GUARD_INSTALL), which glibc 2.40+ uses
          #     for stack guard pages on kernel 6.13+. Numeric because
          #     compile_seccomp_policy.py's constants table predates it.
          #
          # Conditional alternation (not "madvise: 1") is required so
          # jail_warden.policy's own madvise: redefinition still parses —
          # the minijail parser only tolerates redefinition of conditional
          # rules.
          postPatch = (oldAttrs.postPatch or "") + ''
            for policy in jail/seccomp/*/common_device.policy; do
              [ -f "$policy" ] || continue
              sed -i \
                -e 's~^prctl: arg0 == PR_SET_VMA$~prctl: arg0 == PR_SET_VMA || arg0 == PR_SET_NAME~' \
                -e 's~^\(madvise: arg2 ==.*\)$~\1 || arg2 == 102~' \
                "$policy"
            done
          '';

          postInstall = (oldAttrs.postInstall or "") + ''
            # Source dir is whatever cargo's installPhase left us in, which by
            # this point should be the unpacked crosvm tree.
            src_root="$(pwd)"
            compile_script="$src_root/third_party/minijail/tools/compile_seccomp_policy.py"
            if [ ! -f "$compile_script" ]; then
              echo "compile_seccomp_policy.py not found at $compile_script; skipping policy install"
            else
              shopt -s nullglob
              for arch in "$src_root"/jail/seccomp/*/; do
                archname=$(basename "$arch")
                outdir="$out/share/policy/$archname"
                mkdir -p "$outdir"

                # Stage source-form policies with @include paths rewritten to
                # the install dir, so both runtime text parsing AND the BPF
                # compile step below can resolve them without /usr/share.
                for f in "$arch"/*.policy "$arch"/*.json "$arch"/*.frequency; do
                  [ -f "$f" ] || continue
                  cp "$f" "$outdir/"
                done
                find "$outdir" -name '*.policy' -exec \
                  sed -i "s|/usr/share/policy/crosvm|$outdir|g" {} +

                # Patch common_device.policy to allow syscalls that modern
                # glibc/Rust threading emits unconditionally on Linux 6.x+.
                # Without these every device proxy that spawns a worker
                # thread gets SIGSYS during `pthread_create`:
                #
                #   - prctl(PR_SET_NAME): set thread name (Rust's
                #     `thread::Builder::name()` does this from inside the
                #     new thread before user code runs).
                #
                #   - madvise (any advice): the bundled policy only allows
                #     a fixed list of MADV_* constants (DONTNEED / FREE /
                #     MERGEABLE / NOHUGEPAGE / DONTDUMP / REMOVE). On
                #     kernel 6.13+, glibc 2.40+ uses MADV_GUARD_INSTALL
                #     (102) for stack guard pages, which isn't in that
                #     list — every thread spawn dies with SIGSYS as
                #     a result. We widen to `madvise: 1` (advice is a
                #     memory hint, no security-meaningful operation).
                #
                # Filed upstream:
                #   - nixpkgs: NixOS/nixpkgs#517363 (install policies +
                #     widen prctl/madvise in common_device.policy)
                #   - microvm.nix: microvm-nix/microvm.nix#516 (drop
                #     --seccomp-log-failures version-gate, makes the
                #     `version = "107.1-..."` override above unnecessary
                #     once it lands)
                #
                # Apply the same widenings to gpu_common.policy. The GPU
                # device's seccomp filter is layered on gpu_common, which
                # already permits `prctl PR_SET_NAME|PR_GET_NAME`, but
                # still uses the same fixed `madvise: arg2 == ...` list
                # that fails on kernel 6.13+ stack-guard installation.
                # Without this, the gpu device proxy dies the first time
                # a Mesa/EGL worker thread spawns under virgl/cross-domain.
                #
                # `common_device.policy` is already widened by the
                # `postPatch` block above and copied here in widened form;
                # only `gpu_common.policy` still needs the widening at this
                # stage. Keep the `madvise` rule a conditional alternation
                # (rather than `madvise: 1`) so any policy that redefines
                # `madvise` (like `jail_warden.policy`) still parses through
                # `compile_seccomp_policy.py` — minijail only allows
                # redefinition of conditional rules.
                if [ -f "$outdir/gpu_common.policy" ]; then
                  sed -i \
                    -e 's~^prctl: arg0 == PR_SET_VMA$~prctl: arg0 == PR_SET_VMA || arg0 == PR_SET_NAME~' \
                    -e 's~^\(madvise: arg2 ==.*\)$~\1 || arg2 == 102~' \
                    "$outdir/gpu_common.policy"
                fi

                # Pre-compile to .bpf so minijail's runtime can use them in
                # the strict (non-redefinition-tolerant) BPF mode that crosvm
                # prefers when --seccomp-log-failures is not set.
                for policy in "$outdir"/*_device.policy "$outdir"/*_device_vhost_user.policy; do
                  [ -f "$policy" ] || continue
                  bpf="''${policy%.policy}.bpf"
                  ${final.python3}/bin/python3 "$compile_script" \
                    --arch-json "$outdir/constants.json" \
                    --default-action trap \
                    "$policy" "$bpf" || true
                done
              done
            fi
          '';
        });
      });
    };
}
