{
  pkgs,
  packages,
  toolchain,
}: {
  default = pkgs.mkShell {
    buildInputs = [
      toolchain
      pkgs.cargo-outdated
    ];
    packages = [
      pkgs.just
      pkgs.mdbook
      pkgs.zola
      packages.default
      (pkgs.python3.withPackages (ps: [ps.pillow]))
    ];
    shellHook = ''
      echo "Website: cd website && zola serve"
      echo "Documentation: cd docs && mdbook serve"
    '';
  };
}
