{
  microvm,
  nixpkgs,
}:
nixpkgs.lib.composeExtensions microvm.overlay (final: prev: {
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
  # REMOVE-AT-0.4: this whole `crosvm.overrideAttrs` block backs the
  # deprecated `hypervisor = "crosvm"` graphical escape hatch
  # (`docs/src/configuration/graphics-and-games.md`). Schedule: drop
  # together with the hypervisor enum value at the 2026-Q4 re-evaluation
  # window (post QEMU 11.x / Mesa 27.x).
  crosvm = prev.crosvm.overrideAttrs (oldAttrs: {
    buildInputs =
      (oldAttrs.buildInputs or [])
      ++ [
        final.aemu
        final.gfxstream
      ];
    buildFeatures =
      (oldAttrs.buildFeatures or [])
      ++ [
        "gfxstream"
      ];
    nativeBuildInputs =
      (oldAttrs.nativeBuildInputs or [])
      ++ [
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
    postPatch =
      (oldAttrs.postPatch or "")
      + ''
        for policy in jail/seccomp/*/common_device.policy; do
          [ -f "$policy" ] || continue
          sed -i \
            -e 's~^prctl: arg0 == PR_SET_VMA$~prctl: arg0 == PR_SET_VMA || arg0 == PR_SET_NAME~' \
            -e 's~^\(madvise: arg2 ==.*\)$~\1 || arg2 == 102~' \
            "$policy"
        done
      '';

    postInstall =
      (oldAttrs.postInstall or "")
      + ''
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
            #     widen prctl/madvise in common_device.policy) — STILL
            #     OPEN. Once merged and reaching our nixpkgs pin, the
            #     `postPatch` and the policy-install half of
            #     `postInstall` below can be deleted.
            #   - microvm.nix: microvm-nix/microvm.nix#516 (drop
            #     --seccomp-log-failures version-gate) — MERGED
            #     2026-05-07, in our microvm input since the bump to
            #     4fe5ab55 (2026-05-24). That is why this overlay no
            #     longer needs to override `version` to "107.1-...".
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
})
