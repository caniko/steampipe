#!/nix/store/i27rhb3nr65rkrwz36bchkwmav6ggsmn-bash-5.3p9/bin/bash
if [ -z "${XDG_RUNTIME_DIR:-}" ]; then
  export XDG_RUNTIME_DIR="/run/user/$(id -u)"
fi
if [ -z "${WAYLAND_DISPLAY:-}" ]; then
  if [ -S "$XDG_RUNTIME_DIR/wayland-1" ]; then
    export WAYLAND_DISPLAY=wayland-1
  elif [ -S "$XDG_RUNTIME_DIR/wayland-0" ]; then
    export WAYLAND_DISPLAY=wayland-0
  fi
fi
export LD_LIBRARY_PATH=/run/opengl-driver/lib:/nix/store/zs7y2aadk71bawprdcn000az9y05s8nf-vulkan-loader-1.4.341.0/lib:/nix/store/0c0xdj7xpilqfy2p33l1jm407f01652w-libxkbcommon-1.13.1/lib:/nix/store/si4q3zks5mn5jhzzyri9hhd3cv789vlm-gcc-15.2.0-lib/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}
export LIBGL_DRIVERS_PATH=/run/opengl-driver/lib/dri${LIBGL_DRIVERS_PATH:+:$LIBGL_DRIVERS_PATH}
export __EGL_VENDOR_LIBRARY_DIRS=/run/opengl-driver/share/glvnd/egl_vendor.d${__EGL_VENDOR_LIBRARY_DIRS:+:$__EGL_VENDOR_LIBRARY_DIRS}
if [ -d /run/opengl-driver/lib/gbm ]; then
  export GBM_BACKENDS_PATH=/run/opengl-driver/lib/gbm${GBM_BACKENDS_PATH:+:$GBM_BACKENDS_PATH}
fi
export PKG_CONFIG_PATH=/nix/store/lzy227gr54781r8nwws22cw95rl3ji9k-wayland-1.24.0-dev/lib/pkgconfig:/nix/store/s11qidhaq78zrwnd6jc2plxhbmv6y9b9-libxkbcommon-1.13.1-dev/lib/pkgconfig:/nix/store/qc2r5wx0k5dh6ypwqjq4pfy6314npgaj-systemd-minimal-libs-260.1-dev/lib/pkgconfig:/nix/store/arai359zvs0pg7p2gsrq4wlik2kxcq9h-alsa-lib-1.2.15.3-dev/lib/pkgconfig${PKG_CONFIG_PATH:+:$PKG_CONFIG_PATH}

exec /nix/store/m9iqa6dbyl51qlszym9nd37jx2qbcmxj-cluster-ctl-0.1.0/bin/cluster-ctl --vm-count 1  --cluster 1v1-uifull up --runners-dir /nix/store/ss6h9llswnrxrwwmnmb47paq6spsaw6n-cluster-vm-runners-uifull "$@"

