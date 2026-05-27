{
  cargoConfig,
  craneLib,
  cross,
  packages,
  pkgs,
  rsHarborLib,
  rustToolchain,
}:
rsHarborLib.mkDevShells {
  inherit cargoConfig craneLib cross pkgs;

  packages = [
    rustToolchain
    pkgs.cargo-outdated
    pkgs.just
    pkgs.mdbook
    pkgs.zola
    packages.default
    (pkgs.python3.withPackages (ps: [ps.pillow]))
  ];

  extraShellHook = ''
    echo "Website: cd website && zola serve"
    echo "Documentation: cd docs && mdbook serve"
  '';
}
