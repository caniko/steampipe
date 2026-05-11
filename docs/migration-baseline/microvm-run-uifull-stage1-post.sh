#!/nix/store/i27rhb3nr65rkrwz36bchkwmav6ggsmn-bash-5.3p9/bin/bash
set -eou pipefail
rm -f vm-1.sock


rm -f vm-1-gpu.sock
/nix/store/qy6j99wrny4p5j2ds1wncx5lv0ix0a4y-crosvm-107.1-unstable-2026-02-13/bin/crosvm device gpu \
  --socket vm-1-gpu.sock \
  --wayland-sock $XDG_RUNTIME_DIR/$WAYLAND_DISPLAY\
  --params '{"backend":"virglrenderer","context-types":"virgl:virgl2:venus:cross-domain","external-blob":true,"system-blob":true,"udmabuf":true,"implicit-render-server":true,"egl":true,"vulkan":true}' \
  &
while ! [ -S vm-1-gpu.sock ]; do
  sleep .1
done

PATH=$PATH:/nix/store/jjxngswsb214vb58qx485jhmilf0kxxy-coreutils-9.10/bin:/nix/store/775pg3fj605iqgc5wz54g9f11wikrq7f-e2fsprogs-1.47.3-bin/bin
if [ ! -e 'vm-1-home.img' ]; then
  touch 'vm-1-home.img'
  # Mark NOCOW
  chattr +C 'vm-1-home.img' || true
  truncate -s 8192M 'vm-1-home.img'
  mkfs.ext4   'vm-1-home.img'
fi

# Open macvtap interface file descriptors

runtime_args=

exec -a "microvm@vm-1" /nix/store/qy6j99wrny4p5j2ds1wncx5lv0ix0a4y-crosvm-107.1-unstable-2026-02-13/bin/crosvm run -m 2304 -c 1 --serial 'type=stdout,console=true,stdin=true' -p 'console=ttyS0 reboot=k panic=1 8250.nr_uarts=1 net.ifnames=0 loglevel=4 lsm=landlock,yama,bpf init=/nix/store/lk5bdpx3h90qnmakq2sq95iyp5hhfzd7-nixos-system-vm-1-26.05.20260428.e75f257/init regInfo=/nix/store/pvipxnfx9likmlnanm75gd2fw8f7xwbs-closure-info/registration' --no-balloon -r /nix/store/y4a6kbvsn7c5pc5782zzz8n2i68xjdhv-microvm-store-disk.erofs --vhost-user 'gpu,socket=vm-1-gpu.sock' -s vm-1.sock --block 'vm-1-home.img,o_direct=false,ro=false' --net 'tap-name=tap-vm-1,mac=52:54:00:cb:00:01' --vsock 4 --initrd /nix/store/p7n0lz9gl1xsf6j6hh48qqhr1pdi24cw-initrd-linux-6.18.25/initrd /nix/store/dhnyn5fzk59h15qkcggz1fb7dbscg8z4-linux-6.18.25-dev/vmlinux  '--seccomp-policy-dir=/nix/store/qy6j99wrny4p5j2ds1wncx5lv0ix0a4y-crosvm-107.1-unstable-2026-02-13/share/policy/x86_64' --no-usb ${runtime_args:-}

