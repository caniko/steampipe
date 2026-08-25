{
  description = "In-tree test fixture for steampipe cluster validation";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    rs-harbor = {
      url = "git+https://github.com/caniko/harbor-rs.git?ref=trunk&rev=05cc4f162b55fa904b687db1821e2463fa813e50";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    steampipe.url = "path:../..";
    steampipe.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs = {
    self,
    nixpkgs,
    rs-harbor,
    steampipe,
    ...
  }: let
    system = "x86_64-linux";
    pkgs = import nixpkgs {
      inherit system;
      overlays = [steampipe.overlays.default];
    };
    lib = pkgs.lib;
    buildCache = rs-harbor.lib.mkBuildCachePolicy {
      inherit pkgs;
      namespaceScope = "steampipe-fixture";
    };
    clusterCtl = steampipe.packages.${system}.default;
    projectConfig = builtins.fromTOML (builtins.readFile ./steampipe.toml);
    workloads = {
      footBanner = import ./workloads/foot-banner.nix {inherit pkgs lib;};
      vkcubeFrozen = import ./workloads/vkcube-frozen.nix {inherit pkgs;};
      wgpuChecker = import ./workloads/wgpu-checker.nix {inherit pkgs lib buildCache;};
    };
    cluster = steampipe.lib.mkTestCluster {
      inherit pkgs lib nixpkgs clusterCtl steampipe projectConfig;
      projectRoot = ./.;
      extraVmModules = [
        (import ./workloads.nix {
          inherit workloads;
          vmUser = projectConfig.vm_user;
        })
      ];
    };

    copyRuntimeProject = ''
      fixture_root="$(mktemp -d)"
      trap 'rm -rf "$fixture_root"' EXIT
      cp ${./steampipe.toml} "$fixture_root/steampipe.toml"
      install -m 600 ${./cluster_key} "$fixture_root/cluster_key"
      cp ${./cluster_key.pub} "$fixture_root/cluster_key.pub"
    '';

    runnersDirFromScript = script: ''
      runners_dir="$(${pkgs.gnugrep}/bin/grep -oE -- '--runners-dir [^[:space:]]+' ${script} | ${pkgs.gawk}/bin/awk '{ print $2 }' | tail -n1)"
      if [ -z "$runners_dir" ]; then
        echo "failed to extract --runners-dir from ${script}" >&2
        exit 1
      fi
    '';

    mkFixtureApp = name: text:
      pkgs.writeShellApplication {
        inherit name;
        runtimeInputs = [
          clusterCtl
          pkgs.coreutils
          pkgs.gawk
          pkgs.gnugrep
        ];
        inherit text;
      };

    cluster1v1Up = mkFixtureApp "cluster-1v1-up" ''
      set -euo pipefail
      ${copyRuntimeProject}
      ${runnersDirFromScript cluster.scripts."cluster-1v1-up"}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture \
        up \
        --runners-dir "$runners_dir" \
        "$@"
    '';

    cluster1v1Down = mkFixtureApp "cluster-1v1-down" ''
      set -euo pipefail
      ${copyRuntimeProject}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture \
        down \
        "$@"
    '';

    cluster1v1UifullUp = mkFixtureApp "cluster-1v1-uifull-up" ''
      set -euo pipefail
      ${copyRuntimeProject}
      ${runnersDirFromScript cluster.scripts."cluster-1v1-uifull-up"}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture-uifull \
        up \
        --runners-dir "$runners_dir" \
        "$@"
    '';

    cluster1v1UifullDown = mkFixtureApp "cluster-1v1-uifull-down" ''
      set -euo pipefail
      ${copyRuntimeProject}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture-uifull \
        down \
        "$@"
    '';

    # Reuses the tournament runners-dir (vm-1..vm-7 built) but reserves
    # only 1 VM. Lets the diagnostic land on vm-N for any N up to 7
    # depending on which slots are held in the lease pool.
    cluster1v1UifullSoloUp = mkFixtureApp "cluster-1v1-uifull-solo-up" ''
      set -euo pipefail
      ${copyRuntimeProject}
      ${runnersDirFromScript cluster.scripts."cluster-tournament-uifull-up"}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture-uifull-solo \
        up \
        --runners-dir "$runners_dir" \
        "$@"
    '';

    cluster1v1UifullSoloDown = mkFixtureApp "cluster-1v1-uifull-solo-down" ''
      set -euo pipefail
      ${copyRuntimeProject}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture-uifull-solo \
        down \
        "$@"
    '';

    clusterFixtureNetUp = mkFixtureApp "cluster-fixture-net-up" ''
      set -euo pipefail
      ${copyRuntimeProject}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture \
        net-up \
        "$@"
    '';

    clusterFixtureNetDown = mkFixtureApp "cluster-fixture-net-down" ''
      set -euo pipefail
      ${copyRuntimeProject}
      exec ${clusterCtl}/bin/cluster-ctl \
        --project-root "$fixture_root" \
        --vm-count 1 \
        --cluster fixture \
        net-down \
        "$@"
    '';

    mkApp = drv: {
      type = "app";
      program = "${drv}/bin/${drv.name}";
    };
  in {
    packages.${system} = {
      foot-banner = workloads.footBanner;
      vkcube-frozen = workloads.vkcubeFrozen;
      wgpu-checker = workloads.wgpuChecker;
    };

    apps.${system} = {
      cluster-1v1-up = mkApp cluster1v1Up;
      cluster-1v1-down = mkApp cluster1v1Down;
      cluster-1v1-uifull-up = mkApp cluster1v1UifullUp;
      cluster-1v1-uifull-down = mkApp cluster1v1UifullDown;
      cluster-fixture-net-up = mkApp clusterFixtureNetUp;
      cluster-fixture-net-down = mkApp clusterFixtureNetDown;
    };

    "cluster-1v1-up".program = cluster1v1Up;
    "cluster-1v1-down".program = cluster1v1Down;
    "cluster-1v1-uifull-up".program = cluster1v1UifullUp;
    "cluster-1v1-uifull-down".program = cluster1v1UifullDown;
    "cluster-fixture-net-up".program = clusterFixtureNetUp;
    "cluster-fixture-net-down".program = clusterFixtureNetDown;
  };
}
