# Phase 04 — Tolerant diagnostic when SSH command returns empty stdout AND stderr

> **Recommended Codex model: GPT 5.5 medium**
>
> Small Rust change but with a real audit surface: every `bail!` in
> `src/game/steam.rs` and `src/harness/runner.rs` that consumes a
> `run_cmd`'s `stdout`/`stderr` could swallow the diagnostic when the
> VM vanishes mid-command. The agent has to audit those sites and
> wire the `read_vm_log_tail` helper consistently. `low` would patch
> only the one obvious spot in `finish_steam_login`; `high` is
> overkill for what's still ~40 lines of pattern propagation.

## Working tree

`/data/nvme0/can/Projects/steampipe`.

## Goal

When an SSH command issued by cluster-ctl returns `success = false`
with empty `stdout` and empty `stderr`, the error message includes
the tail of the local `vm.log` instead of just two empty parentheses.
This makes hypervisor-level crashes (crosvm VCPU EFAULT, QEMU crash,
KVM error) visible immediately in the cluster-ctl error path
without needing to hand-grep `vm.log`.

## Why this matters now

The 2026-05-26 `cluster-ctl steam login vm-2` transcript showed:

```
Error with vm-2: Steam GUI validation failed (). stderr: . Use VNC fallback if Steam Guard requires GUI confirmation.
```

Empty parens, empty stderr — completely silent about the real
cause (crosvm VCPU EFAULT in vm.log). Without this fix, every
future hypervisor-level crash hides the same way. The whole
hypervisor restructure becomes harder to debug if we ship it before
this lands.

Independent of the hypervisor swap. Lands in Wave 0.

The diagnostic fix is the same shape as the SSH-fix plan's
[Phase 04](../login-runner-ssh-fix/04-vm-log-tail-unavailable-fix.md)
(which fixed the local-file read path); this phase extends the
pattern to the remote-command-output path.

## Out of scope

- Adding new SSH error handling paths or retries. Diagnostics only.
- Changing the wait_ready timeout. Separate concern.
- Refactoring `run_cmd`'s return type. Append context at the call
  sites, don't change the interface.
- Hooking into wayvnc's own logs or anything inside the VM. Only
  the host-side `vm.log` (already written by crosvm/qemu/CHV).

## Plan

1. Factor `print_vm_log_tail` into two parts in
   [src/game/steam.rs:748-770](../../../src/game/steam.rs#L748-L770):

   ```rust
   // Returns the formatted tail (40 lines reversed-and-printed) as
   // a String suitable for embedding in an error message.
   pub fn vm_log_tail(state_dir: &Path, vm: &VmDef) -> Option<String> {
       let path = microvm_log_path(state_dir, vm);
       let bytes = std::fs::read(&path).ok()?;
       let log = String::from_utf8_lossy(&bytes);
       let tail: Vec<&str> = log.lines().rev().take(40).collect();
       Some(tail.into_iter().rev().collect::<Vec<_>>().join("\n"))
   }

   pub fn print_vm_log_tail(state_dir: &Path, vm: &VmDef) {
       match vm_log_tail(state_dir, vm) {
           Some(tail) => {
               let path = microvm_log_path(state_dir, vm);
               println!("  --- {} tail ---", path.display());
               println!("{tail}");
           }
           None => {
               let path = microvm_log_path(state_dir, vm);
               println!("  VM log unavailable: {} (log file does not exist; backend may not have started the VM)", path.display());
           }
       }
   }
   ```

2. In `finish_steam_login` at
   [src/game/steam.rs:1119-1129](../../../src/game/steam.rs#L1119-L1129),
   replace the `bail!` with:

   ```rust
   let output = redact_sensitive(validate.stdout.trim(), secrets);
   let stderr = redact_sensitive(validate.stderr.trim(), secrets);

   if !validate.success || !steam_login_succeeded(&output) {
       if output.is_empty() && stderr.is_empty() {
           let tail = vm_log_tail(&config.state_dir, vm)
               .unwrap_or_else(|| "(vm.log unavailable)".to_string());
           anyhow::bail!(
               "{}: Steam GUI validation produced no output — the VM likely crashed during the call. \
                vm.log tail:\n{tail}",
               vm.name
           );
       }
       anyhow::bail!(
           "{}: Steam GUI validation failed ({}). stderr: {}. Use VNC fallback if Steam Guard requires GUI confirmation.",
           vm.name, output, stderr
       );
   }
   ```

   The function already has `config` in scope (it's the calling
   function's `config.state_dir`); thread it through if needed.

3. **Audit other `bail!` sites that read `run_cmd` output**. Grep:

   ```bash
   grep -nE 'bail!.*\b(stdout|stderr)\b' src/game/steam.rs src/harness/runner.rs src/game/scene/sync.rs src/game/scene/runner.rs
   ```

   For each match, judge:
   - Does this bail format `stdout` and `stderr` directly? If yes,
     and the `success = false` path is the dominant failure mode
     (i.e. it's not "no output is normal here"), apply the same
     empty-AND-empty → log-tail substitution.
   - If the call site doesn't have `state_dir` + `vm` in scope, pass
     them through. Most call sites do already.

   Expected audit count: 3-6 sites. Pattern-propagate, don't refactor
   into a single helper macro — the `bail!` lines vary in tail text
   and that variation carries diagnostic intent.

4. Add a unit test:

   ```rust
   #[test]
   fn empty_run_cmd_output_falls_back_to_vm_log_tail() {
       let tmp = tempdir().unwrap();
       std::fs::create_dir(tmp.path().join("vm-1")).unwrap();
       std::fs::write(
           tmp.path().join("vm-1/vm.log"),
           b"line one\nline two\nlast line\n",
       ).unwrap();
       let vm = VmDef { name: VmName("vm-1".to_string()), index: 1, ..Default::default() };
       let tail = vm_log_tail(tmp.path(), &vm).expect("tail");
       assert!(tail.contains("last line"));
       assert!(tail.contains("line one"));
   }
   ```

5. Verify:

   ```bash
   cargo test --workspace
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --check
   ```

6. Commit:

   ```
   cluster-ctl: surface vm.log tail when SSH command returns no output

   When backend.run_cmd returns success=false with empty stdout and
   stderr, the VM likely crashed (crosvm VCPU EFAULT, QEMU crash,
   etc.) and the diagnostic was lost with the SSH connection.
   Append the local vm.log tail to the error message so the
   underlying cause is visible without manual log-grepping.

   Pattern propagated to <N> other bail! sites in
   src/game/steam.rs and src/harness/runner.rs.
   ```

## Acceptance criteria

- [ ] `vm_log_tail(&Path, &VmDef) -> Option<String>` exists as a
      public helper next to `print_vm_log_tail`.
- [ ] `finish_steam_login`'s error path includes the vm.log tail
      when both stdout and stderr are empty.
- [ ] At least one other `bail!` site in
      `src/game/steam.rs` / `src/harness/runner.rs` gets the same
      treatment (audit; the count goes in the commit message).
- [ ] The unit test `empty_run_cmd_output_falls_back_to_vm_log_tail`
      passes.
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean.
- [ ] Reproducing the vm-2 crash (force timeout or use crosvm-virgl
      with a heavy load) prints actual `vm.log` content in the
      error message, not empty parentheses.
- [ ] The original error message format is preserved when stdout
      *or* stderr is non-empty (don't lose information that already
      worked).

## Files likely touched

- `src/game/steam.rs` — factor `vm_log_tail`, update
  `finish_steam_login`'s bail.
- `src/harness/runner.rs` — apply the same pattern where applicable.
- `src/game/scene/sync.rs` and `src/game/scene/runner.rs` — audit
  for the same pattern; touch only if a real empty-output case
  exists there.

## Pitfalls

- **Over-eager substitution**: not every `bail!` formatting stdout
  + stderr deserves the empty-fallback treatment. Some sites
  legitimately expect "no output, that's fine" — e.g. fire-and-
  forget cleanup. Audit case-by-case; don't blanket-replace.
- **Redaction order**: `vm_log_tail` does not redact secrets (the
  log is hypervisor-level, no user credentials in scope). But if
  any future `vm.log` content could include guest-side stdout
  containing the steam password, you'd need to thread `secrets`
  in. For now: hypervisor logs are clean; document the assumption
  in a code comment.
- **`config.state_dir` plumbing**: a few `bail!` sites may not have
  `config` in scope. Hoist the `state_dir` reference from the
  caller; do not switch to a global. Static config is wrong here.
- **Don't print the tail twice**: if the error path also calls
  `print_vm_log_tail` *and* embeds `vm_log_tail` in the message,
  the operator sees the same log content twice. Pick one:
  embed-in-message is preferred (lets `anyhow` carry the context
  through the call stack).

## Reference

- Design source of truth:
  [../hypervisor-restructure-design.md](../hypervisor-restructure-design.md)
  ("Rust Code Changes" → "Diagnostic tail on empty stdout/stderr").
- Predecessor diagnostic fix:
  [../login-runner-ssh-fix/04-vm-log-tail-unavailable-fix.md](../login-runner-ssh-fix/04-vm-log-tail-unavailable-fix.md)
  — same shape, local file read; this phase extends to remote
  command output.
- Originating symptom: vm-2 transcript
  (`Steam GUI validation failed (). stderr: .`).
