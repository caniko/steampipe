use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const DEFAULT_FIXTURE_REL: &str = "tests/fixture";
const DEFAULT_GOLDEN_REL: &str = "tests/fixture/golden";
const GUEST_IP: &str = "10.0.100.1";
const VM_USER: &str = "fixture";
const WORKLOADS: &[&str] = &["foot-banner", "vkcube-frozen", "wgpu-checker"];

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(default)]
    golden: Vec<GoldenEntry>,
}

#[derive(Debug, Deserialize)]
struct GoldenEntry {
    path: PathBuf,
    sha256: String,
}

pub fn verify(
    project_root: &Path,
    golden_dir: Option<PathBuf>,
    manifest: Option<PathBuf>,
) -> anyhow::Result<()> {
    let golden_dir = resolve_project_path(
        project_root,
        golden_dir.unwrap_or_else(|| PathBuf::from(DEFAULT_GOLDEN_REL)),
    );
    let manifest_path = manifest
        .map(|path| resolve_project_path(project_root, path))
        .unwrap_or_else(|| golden_dir.join("MANIFEST.toml"));

    verify_manifest(&golden_dir, &manifest_path)
}

// Regenerates fixture golden PNGs against the default fixture flavor
// (crosvm + sway). The capture path is `grim` over the in-VM Wayland socket,
// which only works against a wlroots compositor — weston-flavor renders are
// not reproducible here.
pub fn regenerate(
    project_root: &Path,
    fixture: Option<PathBuf>,
    output: Option<PathBuf>,
    allow_dirty: bool,
) -> anyhow::Result<()> {
    let fixture_rel = fixture.unwrap_or_else(|| PathBuf::from(DEFAULT_FIXTURE_REL));
    let fixture_dir = resolve_project_path(project_root, fixture_rel.clone());
    let out_dir = output
        .map(|path| resolve_project_path(project_root, path))
        .unwrap_or_else(|| fixture_dir.join("golden/.candidate"));
    let ssh_key = fixture_dir.join("cluster_key");
    let allow_dirty = allow_dirty || std::env::var("ALLOW_DIRTY").is_ok_and(|value| value == "1");

    require_git_checkout(project_root)?;
    std::fs::create_dir_all(&out_dir)
        .with_context(|| format!("failed to create {}", out_dir.display()))?;

    let steampipe_commit = command_stdout(git(project_root).args(["rev-parse", "HEAD"]))?;
    let steampipe_dirty =
        !command_stdout(git(project_root).args(["status", "--short"]))?.is_empty();
    let fixture_lock = fixture_rel.join("flake.lock");
    let fixture_flake_lock_committed = git_path_clean(project_root, &fixture_lock)?;

    if steampipe_dirty && !allow_dirty {
        anyhow::bail!(
            "refusing to capture from a dirty steampipe checkout; commit or stash changes, or rerun with --allow-dirty or ALLOW_DIRTY=1 for non-canonical candidate capture"
        );
    }
    if !fixture_flake_lock_committed && !allow_dirty {
        anyhow::bail!(
            "refusing to capture with an uncommitted {}; commit the lockfile, or rerun with --allow-dirty or ALLOW_DIRTY=1 for non-canonical candidate capture",
            fixture_lock.display()
        );
    }
    require_passwordless_sudo()?;

    let fixture_flake_lock_sha = command_stdout(
        git(project_root)
            .arg("hash-object")
            .arg(fixture_dir.join("flake.lock")),
    )?;
    let nixpkgs_rev = fixture_nixpkgs_rev(&fixture_dir)?;
    let host_nixpkgs_divergence = if Path::new("/etc/NIXOS").exists() {
        "host-nixos-unchecked"
    } else {
        "unknown"
    };
    println!(
        "capture-note steampipe_commit={} dirty={} fixture_flake_lock_committed={} fixture_flake_lock_sha={} fixture_nixpkgs_rev={} host_nixpkgs={}",
        steampipe_commit.trim(),
        if steampipe_dirty { "yes" } else { "no" },
        if fixture_flake_lock_committed {
            "yes"
        } else {
            "no"
        },
        fixture_flake_lock_sha.trim(),
        nixpkgs_rev.trim(),
        host_nixpkgs_divergence
    );

    let _cleanup = Cleanup::new(project_root.to_path_buf(), fixture_rel.clone());
    run_checked(
        Command::new("sudo")
            .arg("nix")
            .arg("run")
            .arg(format!(
                "./{}#cluster-fixture-net-up",
                fixture_rel.display()
            ))
            .current_dir(project_root),
    )?;
    run_checked(
        Command::new("nix")
            .arg("run")
            .arg(format!("./{}#cluster-1v1-up", fixture_rel.display()))
            .current_dir(project_root),
    )?;

    wait_for_ssh(&ssh_key)?;
    for workload in WORKLOADS {
        ssh(&ssh_key)
            .arg(format!("workload-stop || true; workload-run '{workload}'"))
            .status()
            .with_context(|| format!("failed to start workload {workload} over ssh"))?
            .success()
            .then_some(())
            .ok_or_else(|| anyhow!("ssh workload command failed for {workload}"))?;
        std::thread::sleep(Duration::from_secs(3));

        run_checked(ssh(&ssh_key).arg(
            "export XDG_RUNTIME_DIR=/tmp/runtime-fixture; export WAYLAND_DISPLAY=wayland-1; swaymsg -q seat seat0 cursor set 5000 5000; grim /tmp/grim.png",
        ))?;

        let candidate = out_dir.join(format!("{workload}-1280x720.png"));
        run_checked(
            Command::new("rsync")
                .arg("-e")
                .arg(ssh_transport(&ssh_key))
                .arg(format!("{VM_USER}@{GUEST_IP}:/tmp/grim.png"))
                .arg(&candidate),
        )?;
        run_checked(
            Command::new("oxipng")
                .arg("-o4")
                .arg("--strip")
                .arg("all")
                .arg(&candidate)
                .stdout(Stdio::null()),
        )?;
    }

    print_png_report(&out_dir)
}

fn verify_manifest(golden_dir: &Path, manifest_path: &Path) -> anyhow::Result<()> {
    if !manifest_path.is_file() {
        anyhow::bail!("missing manifest: {}", manifest_path.display());
    }

    let manifest = std::fs::read_to_string(manifest_path)
        .with_context(|| format!("failed to read {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;

    let mut failures = Vec::new();
    for entry in manifest.golden {
        let path = golden_dir.join(&entry.path);
        if !path.is_file() {
            failures.push(format!("missing file: {}", path.display()));
            continue;
        }
        let digest = sha256_file(&path)?;
        if digest != entry.sha256 {
            failures.push(format!(
                "sha256 mismatch for {}: expected {} got {}",
                entry.path.display(),
                entry.sha256,
                digest
            ));
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    for failure in &failures {
        eprintln!("{failure}");
    }
    anyhow::bail!(
        "golden verification failed with {} failure(s)",
        failures.len()
    );
}

fn print_png_report(out_dir: &Path) -> anyhow::Result<()> {
    let mut paths = std::fs::read_dir(out_dir)
        .with_context(|| format!("failed to read {}", out_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "png"));
    paths.sort();

    for path in paths {
        let metadata = png_metadata(&path)?;
        println!(
            "{}\tsha256={}\tbytes={}\twidth={}\theight={}\tbit_depth={}\tcolor_type={}\tcompression={}\tfilter={}\tinterlace={}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("<invalid-utf8>"),
            sha256_file(&path)?,
            path.metadata()?.len(),
            metadata.width,
            metadata.height,
            metadata.bit_depth,
            metadata.color_type,
            metadata.compression,
            metadata.filter,
            metadata.interlace
        );
    }
    Ok(())
}

fn png_metadata(path: &Path) -> anyhow::Result<PngMetadata> {
    let raw = std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if raw.len() < 33 || &raw[..8] != b"\x89PNG\r\n\x1a\n" {
        anyhow::bail!("{}: not a PNG", path.display());
    }
    if &raw[12..16] != b"IHDR" {
        anyhow::bail!("{}: missing IHDR", path.display());
    }
    Ok(PngMetadata {
        width: u32::from_be_bytes(raw[16..20].try_into()?),
        height: u32::from_be_bytes(raw[20..24].try_into()?),
        bit_depth: raw[24],
        color_type: raw[25],
        compression: raw[26],
        filter: raw[27],
        interlace: raw[28],
    })
}

#[derive(Debug)]
struct PngMetadata {
    width: u32,
    height: u32,
    bit_depth: u8,
    color_type: u8,
    compression: u8,
    filter: u8,
    interlace: u8,
}

fn fixture_nixpkgs_rev(fixture_dir: &Path) -> anyhow::Result<String> {
    let output = command_stdout(
        Command::new("nix")
            .arg("flake")
            .arg("metadata")
            .arg("--json")
            .arg(fixture_dir),
    )?;
    let value: serde_json::Value = serde_json::from_str(&output)?;
    value
        .pointer("/locks/nodes/nixpkgs/locked/rev")
        .and_then(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("failed to read nixpkgs locked rev from fixture flake metadata"))
}

fn wait_for_ssh(ssh_key: &Path) -> anyhow::Result<()> {
    for _ in 0..60 {
        if ssh(ssh_key)
            .arg("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }

    if ssh(ssh_key)
        .arg("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
    {
        return Ok(());
    }
    anyhow::bail!("fixture VM did not become reachable over SSH");
}

fn ssh(ssh_key: &Path) -> Command {
    let mut command = Command::new("ssh");
    command
        .arg("-i")
        .arg(ssh_key)
        .args([
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "UserKnownHostsFile=/dev/null",
            "-o",
            "ConnectTimeout=5",
        ])
        .arg(format!("{VM_USER}@{GUEST_IP}"));
    command
}

fn ssh_transport(ssh_key: &Path) -> String {
    format!(
        "ssh -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5",
        ssh_key.display()
    )
}

fn require_git_checkout(project_root: &Path) -> anyhow::Result<()> {
    run_checked(
        git(project_root)
            .args(["rev-parse", "--is-inside-work-tree"])
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map_err(|_| anyhow!("fixture golden regenerate must run from a steampipe git checkout"))
}

fn git_path_clean(project_root: &Path, path: &Path) -> anyhow::Result<bool> {
    let cached = git(project_root)
        .args(["diff", "--cached", "--quiet", "--"])
        .arg(path)
        .status()?;
    let worktree = git(project_root)
        .args(["diff", "--quiet", "--"])
        .arg(path)
        .status()?;
    Ok(cached.success() && worktree.success())
}

fn require_passwordless_sudo() -> anyhow::Result<()> {
    run_checked(
        Command::new("sudo")
            .arg("-n")
            .arg("true")
            .stdout(Stdio::null())
            .stderr(Stdio::null()),
    )
    .map_err(|_| anyhow!("passwordless sudo is required for cluster-fixture-net-up/net-down; validate with: sudo -n true"))
}

fn git(project_root: &Path) -> Command {
    let mut command = Command::new("git");
    command.arg("-C").arg(project_root);
    command
}

fn command_stdout(command: &mut Command) -> anyhow::Result<String> {
    let output = command.output().with_context(|| {
        format!(
            "failed to execute {}",
            command.get_program().to_string_lossy()
        )
    })?;
    if !output.status.success() {
        anyhow::bail!(
            "command failed: {} {}",
            command.get_program().to_string_lossy(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    Ok(String::from_utf8(output.stdout)?)
}

fn run_checked(command: &mut Command) -> anyhow::Result<()> {
    let status = command.status().with_context(|| {
        format!(
            "failed to execute {}",
            command.get_program().to_string_lossy()
        )
    })?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!(
            "command failed with {status}: {} {}",
            command.get_program().to_string_lossy(),
            command
                .get_args()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        )
    }
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    let digest = Sha256::digest(bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn resolve_project_path(project_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

struct Cleanup {
    project_root: PathBuf,
    fixture_rel: PathBuf,
    current_exe: PathBuf,
}

impl Cleanup {
    fn new(project_root: PathBuf, fixture_rel: PathBuf) -> Self {
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("cluster-ctl"));
        Self {
            project_root,
            fixture_rel,
            current_exe,
        }
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = Command::new(&self.current_exe)
            .arg("--project-root")
            .arg(&self.fixture_rel)
            .args(["--vm-count", "1", "down"])
            .current_dir(&self.project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("sudo")
            .arg(&self.current_exe)
            .arg("--project-root")
            .arg(&self.fixture_rel)
            .args(["--vm-count", "1", "net-down"])
            .current_dir(&self.project_root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_manifest_reports_missing_manifest() {
        let tmp = tempfile::tempdir().unwrap();
        let err = verify_manifest(tmp.path(), &tmp.path().join("MANIFEST.toml"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing manifest"));
    }

    #[test]
    fn verify_manifest_reports_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let manifest = tmp.path().join("MANIFEST.toml");
        std::fs::write(
            &manifest,
            r#"
[[golden]]
path = "missing.png"
sha256 = "abc"
"#,
        )
        .unwrap();

        let err = verify_manifest(tmp.path(), &manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("golden verification failed"));
    }

    #[test]
    fn verify_manifest_reports_hash_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("actual.png"), b"not really png").unwrap();
        let manifest = tmp.path().join("MANIFEST.toml");
        std::fs::write(
            &manifest,
            r#"
[[golden]]
path = "actual.png"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000"
"#,
        )
        .unwrap();

        let err = verify_manifest(tmp.path(), &manifest)
            .unwrap_err()
            .to_string();
        assert!(err.contains("golden verification failed"));
    }

    #[test]
    fn verify_manifest_accepts_matching_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("actual.png");
        std::fs::write(&path, b"fixture bytes").unwrap();
        let digest = sha256_file(&path).unwrap();
        let manifest = tmp.path().join("MANIFEST.toml");
        std::fs::write(
            &manifest,
            format!(
                r#"
[[golden]]
path = "actual.png"
sha256 = "{digest}"
"#
            ),
        )
        .unwrap();

        verify_manifest(tmp.path(), &manifest).unwrap();
    }
}
