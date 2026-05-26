# Phase 04 — Fix `print_vm_log_tail` reading "VM log unavailable" when it exists

> **Recommended Codex model: GPT 5.5 medium**
>
> Small Rust diagnosis with a real read-vs-write race or path-mismatch
> hiding behind it. The fix is probably ≤ 10 lines, but the agent has
> to reproduce or convincingly explain the race before committing
> anything. `low` would skip the diagnosis and apply a cargo-cult fix;
> `high` is unnecessary for a single function in `src/game/steam.rs`.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

After this phase, when `cluster-ctl steam login` times out waiting for
SSH, `print_vm_log_tail` either prints the tail of the actual VM log
that exists on disk, or — if the file genuinely doesn't exist —
explains *why* (e.g., "backend did not start the VM") instead of
silently reporting "VM log unavailable" against a path that does exist.

## Why this matters now

The chat transcript that opened this investigation shows:

```
  Waiting for SSH... timeout after 180s
  VM log unavailable: /home/can/.local/state/steampipe/default/vm-1/vm.log
Error with vm-1: SSH timeout for vm-1
```

But the file at that exact path was 15 584 bytes at the time, with a
mtime matching the run. The "unavailable" message is wrong and it
ate the operator's most useful diagnostic when the actual SSH bug
was hiding.

Relevant code:
[src/game/steam.rs:748-753](../../../src/game/steam.rs#L748-L753)

```rust
fn print_vm_log_tail(state_dir: &Path, vm_name: &str) {
    let path = state_dir.join(vm_name).join("vm.log");
    let Ok(log) = std::fs::read_to_string(&path) else {
        println!("  VM log unavailable: {}", path.display());
        return;
    };
    ...
}
```

The write path is at
[src/core/backend.rs:256](../../../src/core/backend.rs#L256) and the
canonical accessor is `microvm_log_path` at
[src/core/backend.rs:305-306](../../../src/core/backend.rs#L305-L306).
The reader hand-computes the same path instead of calling the
accessor — possibly the bug, possibly fine.

Candidate hypotheses (the phase should disambiguate before fixing):

1. **Encoding issue**: the log contains non-UTF-8 bytes (ANSI escapes
   are UTF-8 safe but the VM's serial output could embed raw bytes
   from kernel messages). `read_to_string` fails on non-UTF-8 and
   returns an `Err`, but the file exists.
2. **Race with `stop_instance`**: `print_vm_log_tail` is called
   before `cleanup()`, but if some other concurrent task has already
   stopped the VM and the file got recreated or truncated, the read
   may see a transient state.
3. **Path mismatch**: the reader uses `state_dir.join(vm_name)` while
   the writer uses `microvm_log_path(state_dir, vm)` — if these
   diverge for any future backend, the reader silently misses the
   file.
4. **Permission issue**: the file is created by the crosvm process;
   if that process drops privileges differently than expected, the
   file may not be readable by the cluster-ctl process.

## Out of scope

- Replacing `println!` with structured logging across the binary. Not
  this phase's job.
- Adding new diagnostic output for the SSH failure itself (the wait
  loop, the route to the VM, etc.).
- Tail length tuning — the existing 40-line tail is fine.

## Plan

1. Read the writer side carefully:

   ```bash
   sed -n '250,310p' src/core/backend.rs
   ```

   Note exactly which `state_dir` and `vm` shape produces the path,
   and whether the writer holds the file open after `start_instance`
   returns.

2. Read the reader side:

   ```bash
   sed -n '740,775p' src/game/steam.rs
   ```

   Confirm `state_dir` and `vm_name` are passed through unchanged
   from the caller at `steam.rs:711`.

3. Reproduce the failure locally. With the bug from P1/P2 still
   active (or with a forced timeout via a tiny `LOGIN_SSH_TIMEOUT_SECS`
   value), run `cluster-ctl steam login vm-1` and confirm
   "VM log unavailable" is printed against a real 15 KiB file.

4. Diagnose. The fastest disambiguator: replace the `let Ok(log) =`
   pattern temporarily with explicit `match std::fs::read(&path)`,
   print the `io::Error` if `Err`, and the byte count if `Ok`. Run
   again to see whether the failure is `NotFound`, `PermissionDenied`,
   or an `InvalidData` from `read_to_string`'s UTF-8 validation.

   - If `InvalidData` → root cause is hypothesis #1. Switch the
     reader to `std::fs::read(&path)` and `String::from_utf8_lossy`
     for the tail print. The diagnostic doesn't need to be valid
     UTF-8.
   - If `NotFound` → root cause is hypothesis #2 or #3. Inspect
     mtime/ctime ordering; check whether `cleanup()` runs before
     `print_vm_log_tail` (it shouldn't per
     [steam.rs:708-713](../../../src/game/steam.rs#L708-L713)) and
     verify by adding a debug print of `path.exists()` immediately
     before the read.
   - If `PermissionDenied` → hypothesis #4. Check the file's owner.

5. Apply the targeted fix.

   - Most likely fix path: use `std::fs::read` + `String::from_utf8_lossy`
     so non-UTF-8 bytes don't take the diagnostic offline. ANSI escapes
     in the log are already noisy; the existing tail printer at
     [src/game/steam.rs:754-762](../../../src/game/steam.rs#L754-L762)
     can iterate over the lossy string just as easily.
   - Also use `microvm_log_path` (the canonical accessor) instead of
     re-deriving the path. Requires passing `&VmDef` (or equivalent)
     to `print_vm_log_tail` rather than `vm_name: &str`. Adjust the
     call site at `steam.rs:711`.

6. Add a regression test if practical. The existing unit test at
   [src/core/backend.rs:587-588](../../../src/core/backend.rs#L587-L588)
   already pins `microvm_log_path`'s output for a known input —
   extend it (or add a sibling test in `steam.rs`) that writes a
   file containing non-UTF-8 bytes and asserts `print_vm_log_tail`
   prints something other than "VM log unavailable".

7. `cargo test --workspace`, `cargo clippy --workspace --all-targets
   -- -D warnings`, `cargo fmt --check`.

8. Commit:

   ```
   cluster-ctl: print VM log tail tolerantly

   read_to_string returned InvalidData on non-UTF-8 kernel output, so
   the SSH-timeout diagnostic was eaten and printed "VM log
   unavailable" against a 15 KiB file that did exist. Switch to
   read + from_utf8_lossy and use microvm_log_path so the reader and
   writer can't drift.
   ```

   (Adjust the message body to whichever root cause your diagnosis
   actually finds.)

## Acceptance criteria

- [ ] Reproducing the SSH-timeout scenario (force a timeout via a
      small `LOGIN_SSH_TIMEOUT_SECS` or by leaving the empty-key
      bug in place on a scratch host) prints actual log content,
      not "VM log unavailable".
- [ ] `print_vm_log_tail` uses `microvm_log_path` (or a single
      accessor) for the path; the path is not hand-derived in two
      places.
- [ ] The commit message names the diagnosed root cause (encoding
      vs path-mismatch vs race vs permissions) — not "be more
      defensive".
- [ ] `cargo test --workspace` passes; new test (if added) exercises
      the previously broken code path.
- [ ] No new clippy warnings; `cargo fmt --check` clean.

## Files likely touched

- `src/game/steam.rs` — `print_vm_log_tail` and its call site.
- `src/core/backend.rs` — possibly export
  `microvm_log_path` more cleanly, or no change if it's already
  reachable.
- Possibly a small new test in `src/core/backend.rs` or
  `src/game/steam.rs`.

## Pitfalls

- **Treating the symptom**: just removing the `else` branch's
  "VM log unavailable" print makes the bug invisible without fixing
  it. The fix needs to actually print log content.
- **Re-deriving paths in a third place**: if you add yet another
  ad-hoc `state_dir.join(vm_name).join("vm.log")` somewhere "for
  clarity", you've made the reader/writer drift more likely, not
  less. Always go through `microvm_log_path`.
- **Assuming the file is UTF-8**: kernel ring-buffer dumps over the
  serial console can contain arbitrary bytes. Default to
  `String::from_utf8_lossy` for any diagnostic print.
- **Hidden caller in `harness/runner.rs`**: a separate code path at
  [src/harness/runner.rs:1264](../../../src/harness/runner.rs#L1264)
  uses a different `collect_vm_log` that goes over SSH. Don't conflate
  the two — `print_vm_log_tail` is the local-file path, intentionally
  available when SSH is broken.

## Reference

- Findings note's "Optional follow-ups" section in
  [../login-runner-ssh-unreachable-findings.md](../login-runner-ssh-unreachable-findings.md).
- Path accessor:
  [src/core/backend.rs:305-306](../../../src/core/backend.rs#L305-L306).
- Existing test that pins the accessor:
  [src/core/backend.rs:587-599](../../../src/core/backend.rs#L587-L599).
