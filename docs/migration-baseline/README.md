# Cluster VM stage-1 migration baseline

Captured on 2026-05-11 from the downstream regicide checkout after bumping
its `steampipe` input to Phase D (`aa22dcb0cd117843a0252c872e5ac53ac9879b25`).

The baseline VM is the crosvm UI-full flavor:

```sh
cd /data/nvme0/can/Projects/solo/game-dev/regicide
nix build --no-link --print-out-paths .#apps.x86_64-linux.cluster-1v1-uifull-up.program
nix run .#cluster-1v1-uifull-up
```

Files:

- `cluster-1v1-uifull-eval-pre.log`: evaluation output after the Phase D lock bump. The crosvm version warning is gone and the scripted-initrd warning remains.
- `cluster-1v1-uifull-up-pre.log`: baseline boot command output.
- `cluster-1v1-uifull-up-pre.sh`: generated wrapper script for the pre-migration UI-full runner.
- `microvm-run-uifull-pre.sh`: generated crosvm `microvm-run` script for `vm-1`.
- `journal-pre.txt`, `analyze-blame-pre.txt`, `analyze-chain-pre.txt`, `failed-units-pre.txt`, `mounts-pre.txt`, `modules-pre.txt`, `network-pre.txt`, `dri-pre.txt`, `vhost-gpu-pre.txt`: captured from the booted VM over SSH.
- `*-post.txt`: same captures from the `uifull-stage1` flavor after the
  systemd-stage-1 branch booted successfully.
- `diffs/*.diff`: unified diffs from pre-migration scripted initrd to
  post-migration systemd stage-1.
- `smoke-status.md`: E3/E4 downstream smoke result and why the default was not
  flipped.

Important observed facts:

- `failed-units-pre.txt` reports `0 loaded units listed`.
- `modules-pre.txt` includes `virtio_gpu`.
- `dri-pre.txt` includes `/dev/dri/card0` and `/dev/dri/renderD128`.
- `vhost-gpu-pre.txt` is empty inside the guest. The vhost-user GPU socket is host-side.
- `microvm-run-uifull-pre.sh` removes and waits for `vm-1-gpu.sock` before starting `crosvm run`.

Important post-migration facts:

- `failed-units-post.txt` reports `0 loaded units listed`.
- `modules-post.txt` includes both `virtio_gpu` and `erofs`.
- `dri-post.txt` includes `/dev/dri/card0` and `/dev/dri/renderD128`.
- `vhost-gpu-post.txt` is empty inside the guest; the host-side GPU socket
  wait remains in `microvm-run-uifull-stage1-post.sh`.
- The crosvm systemd stage-1 branch needs an explicit initrd `sysroot.mount`
  tmpfs unit. Leaving `root=fstab` in place makes systemd select `/dev/vda`
  as `/sysroot`, which is the read-only EROFS store disk.
