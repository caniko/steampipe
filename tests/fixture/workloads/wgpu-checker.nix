{pkgs, lib}:
pkgs.rustPlatform.buildRustPackage {
  pname = "wgpu-checker";
  version = "0.1.0";
  src = lib.cleanSource ./wgpu-checker;
  cargoLock = {
    lockFile = ./wgpu-checker/Cargo.lock;
  };
  nativeBuildInputs = [pkgs.makeWrapper];
  buildInputs = [
    pkgs.libxkbcommon
    pkgs.vulkan-loader
    pkgs.wayland
  ];
  postInstall = ''
    wrapProgram "$out/bin/wgpu-checker" \
      --prefix LD_LIBRARY_PATH : /run/opengl-driver/lib
  '';
}
