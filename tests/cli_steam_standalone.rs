use std::fs;
use std::path::Path;

use serial_test::serial;
use tempfile::TempDir;

use serde_json::json;

fn command(root: &Path) -> assert_cmd::Command {
    let xdg_config_home = root.join("xdg-config");
    let xdg_state_home = root.join("xdg-state");
    let xdg_runtime_dir = root.join("xdg-runtime");
    let home = root.join("home");
    let etc_root = root.join("etc");

    fs::create_dir_all(xdg_config_home.join("steampipe")).expect("xdg config dir");
    fs::create_dir_all(&xdg_state_home).expect("xdg state dir");
    fs::create_dir_all(&xdg_runtime_dir).expect("xdg runtime dir");
    fs::create_dir_all(&home).expect("home dir");
    fs::create_dir_all(&etc_root).expect("etc root");
    fs::write(xdg_config_home.join("steampipe/cluster_key"), "dummy").expect("cluster key");

    let mut command = assert_cmd::Command::cargo_bin("cluster-ctl").expect("cluster-ctl binary");
    command
        .current_dir(root)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .env("XDG_STATE_HOME", &xdg_state_home)
        .env("XDG_RUNTIME_DIR", &xdg_runtime_dir);
    command.env("STEAMPIPE_ETC_DIR", &etc_root);
    command
}

fn command_with_etc(root: &Path, etc_root: &Path) -> assert_cmd::Command {
    let mut command = command(root);
    command.env("STEAMPIPE_ETC_DIR", etc_root);
    command
}

fn write_host_json(path: &Path, vm_count: u8, accounts: &[(&str, &str)], login_state_dir: &Path) {
    write_host_json_with_schema(path, 2, vm_count, accounts, login_state_dir);
}

fn write_host_json_with_schema(
    path: &Path,
    schema_version: u8,
    vm_count: u8,
    accounts: &[(&str, &str)],
    login_state_dir: &Path,
) {
    let accounts = accounts
        .iter()
        .map(|(vm, steam_user)| json!({ "vm": vm, "steamUser": steam_user }))
        .collect::<Vec<_>>();
    let contents = json!({
        "schemaVersion": schema_version,
        "vmCount": vm_count,
        "bridge": "br-cluster",
        "subnet": "10.0.100",
        "prefix": 24,
        "hostIp": "10.0.100.254",
        "tapOwner": null,
        "loginStateDir": login_state_dir,
        "credentialsPath": null,
        "accounts": accounts,
    });
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("host config parent");
    }
    fs::write(
        path,
        serde_json::to_string(&contents).expect("serialize host config"),
    )
    .expect("write host config");
}

fn write_project_root(root: &Path) {
    fs::write(
        root.join("steampipe.toml"),
        r#"
binary_name = "my-game"
cargo_package = "my-game"
vm_user = "player"
remote_dir = "/home/player/game"
"#,
    )
    .expect("write steampipe.toml");
}

fn write_claims(lock_dir: &Path, cluster: &str, ids: &[u8]) {
    fs::create_dir_all(lock_dir).expect("lock dir");
    for id in ids {
        let claim = json!({
            "pid": std::process::id(),
            "cluster": cluster,
            "since": "2026-05-17T00:00:00+00:00",
        });
        fs::write(lock_dir.join(format!("vm-{id}.claim")), claim.to_string()).expect("write claim");
    }
}

fn output_string(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
#[serial]
fn discovery_cli_flag_overrides_all_locations() {
    let temp = TempDir::new().expect("tempdir");
    let explicit = temp.path().join("explicit.json");
    let login_state_dir = temp.path().join("explicit-logins");
    fs::create_dir_all(&login_state_dir).expect("login state dir");
    write_host_json(
        &explicit,
        1,
        &[("vm-1", "explicit-account")],
        &login_state_dir,
    );

    let xdg_host = temp.path().join("xdg-config/steampipe/host.json");
    let etc_root = temp.path().join("etc");
    let etc_host = etc_root.join("steampipe/host.json");
    let etc_module = etc_root.join("steampipe/module.json");
    write_host_json(
        &xdg_host,
        2,
        &[("vm-1", "xdg-account")],
        &temp.path().join("xdg-logins"),
    );
    write_host_json(
        &etc_host,
        3,
        &[("vm-1", "etc-account")],
        &temp.path().join("etc-logins"),
    );
    write_host_json(
        &etc_module,
        4,
        &[("vm-1", "legacy-account")],
        &temp.path().join("legacy-logins"),
    );

    let output = command_with_etc(temp.path(), &etc_root)
        .args([
            "--host-config",
            explicit.to_str().expect("utf-8 path"),
            "steam",
            "info",
        ])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(
        stdout.contains(&format!("Host config source: {}", explicit.display())),
        "{stdout}"
    );
    assert_eq!(stdout.matches("(selected)").count(), 1, "{stdout}");
    assert!(stdout.contains("  1. --host-config flag"), "{stdout}");
    assert!(stdout.contains(&explicit.display().to_string()), "{stdout}");
}

#[test]
#[serial]
fn discovery_xdg_beats_etc_host_json() {
    let temp = TempDir::new().expect("tempdir");
    let login_state_dir = temp.path().join("xdg-logins");
    let xdg_host = temp.path().join("xdg-config/steampipe/host.json");
    let etc_root = temp.path().join("etc");
    let etc_host = etc_root.join("steampipe/host.json");
    fs::create_dir_all(&login_state_dir).expect("login state dir");
    write_host_json(&xdg_host, 2, &[("vm-1", "xdg-account")], &login_state_dir);
    write_host_json(
        &etc_host,
        5,
        &[("vm-1", "etc-account")],
        &temp.path().join("etc-logins"),
    );

    let output = command_with_etc(temp.path(), &etc_root)
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains("Host config source: "), "{stdout}");
    assert!(stdout.contains(&xdg_host.display().to_string()), "{stdout}");
    assert!(stdout.contains("  2. "), "{stdout}");
    assert!(stdout.contains("(selected)"), "{stdout}");
    assert!(stdout.contains(&etc_host.display().to_string()), "{stdout}");
    assert!(stdout.contains("(not found)"), "{stdout}");
    assert!(stdout.contains("vmCount         2"), "{stdout}");
}

#[test]
#[serial]
fn discovery_etc_host_json_beats_etc_module_json() {
    let temp = TempDir::new().expect("tempdir");
    let etc_root = temp.path().join("etc");
    let etc_host = etc_root.join("steampipe/host.json");
    let etc_module = etc_root.join("steampipe/module.json");
    let etc_login_state_dir = temp.path().join("etc-logins");
    fs::create_dir_all(&etc_login_state_dir).expect("login state dir");
    write_host_json(
        &etc_host,
        3,
        &[("vm-1", "host-account")],
        &etc_login_state_dir,
    );
    write_host_json(
        &etc_module,
        6,
        &[("vm-1", "module-account")],
        &temp.path().join("module-logins"),
    );

    let output = command_with_etc(temp.path(), &etc_root)
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains(&etc_host.display().to_string()), "{stdout}");
    assert!(stdout.contains("  3. "), "{stdout}");
    assert!(stdout.contains("(selected)"), "{stdout}");
    assert!(
        stdout.contains(&etc_module.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("(not found)"), "{stdout}");
    assert!(stdout.contains("vmCount         3"), "{stdout}");
}

#[test]
#[serial]
fn discovery_falls_back_to_legacy_module_json() {
    let temp = TempDir::new().expect("tempdir");
    let etc_root = temp.path().join("etc");
    let etc_module = etc_root.join("steampipe/module.json");
    write_host_json(
        &etc_module,
        4,
        &[("vm-1", "legacy-account")],
        &temp.path().join("legacy-logins"),
    );

    let output = command_with_etc(temp.path(), &etc_root)
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(
        stdout.contains(&etc_module.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("  4. "), "{stdout}");
    assert!(stdout.contains("(selected)"), "{stdout}");
    assert!(stdout.contains("vmCount         4"), "{stdout}");
}

#[test]
#[serial]
fn schema_one_host_config_warns_but_still_loads() {
    let temp = TempDir::new().expect("tempdir");
    let host_path = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json_with_schema(
        &host_path,
        1,
        1,
        &[("vm-1", "legacy-schema-account")],
        &temp.path().join("schema-1-logins"),
    );

    let output = command(temp.path())
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains("schemaVersion 1"), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("uses schemaVersion 1; upgrade to 3"),
        "{stderr}"
    );
}

#[test]
#[serial]
fn discovery_none_present_falls_through_to_project_root_error() {
    let temp = TempDir::new().expect("tempdir");

    let output = command(temp.path())
        .args(["steam", "accounts"])
        .output()
        .expect("run cluster-ctl");

    assert!(!output.status.success(), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Could not find project root"), "{stderr}");
}

#[test]
#[serial]
fn info_with_hand_written_xdg_config_renders_trace() {
    let temp = TempDir::new().expect("tempdir");
    let login_state_dir = temp.path().join("logins");
    fs::create_dir_all(&login_state_dir).expect("login state dir");
    let xdg_host = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json(
        &xdg_host,
        2,
        &[("vm-1", "alice"), ("vm-2", "bob")],
        &login_state_dir,
    );

    let output = command(temp.path())
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains(&xdg_host.display().to_string()), "{stdout}");
    assert!(stdout.contains("  2. "), "{stdout}");
    assert!(stdout.contains("(selected)"), "{stdout}");
    assert!(stdout.contains("Cluster shape:"), "{stdout}");
    assert!(stdout.contains("Configured accounts (2):"), "{stdout}");
}

#[test]
#[serial]
fn info_with_legacy_module_json_renders_trace() {
    let temp = TempDir::new().expect("tempdir");
    let etc_root = temp.path().join("etc");
    let etc_module = etc_root.join("steampipe/module.json");
    write_host_json(
        &etc_module,
        4,
        &[("vm-1", "alice"), ("vm-2", "bob")],
        &temp.path().join("logins"),
    );

    let output = command_with_etc(temp.path(), &etc_root)
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(
        stdout.contains(&etc_module.display().to_string()),
        "{stdout}"
    );
    assert!(stdout.contains("  4. "), "{stdout}");
    assert!(stdout.contains("(selected)"), "{stdout}");
}

#[test]
#[serial]
fn info_without_any_config_renders_trace_with_no_selection() {
    let temp = TempDir::new().expect("tempdir");

    let output = command(temp.path())
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(
        stdout.contains("Host config source: no config discovered"),
        "{stdout}"
    );
    assert!(stdout.contains("(not set)"), "{stdout}");
    assert!(stdout.contains("(not found)"), "{stdout}");
    assert!(stdout.contains("Discovery trace:"), "{stdout}");
}

#[test]
#[serial]
fn accounts_without_lease_uses_host_config_accounts() {
    let temp = TempDir::new().expect("tempdir");
    let login_state_dir = temp.path().join("logins");
    fs::create_dir_all(&login_state_dir).expect("login state dir");
    let host_path = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json(
        &host_path,
        3,
        &[("vm-1", "alice"), ("vm-2", "bob"), ("vm-3", "carol")],
        &login_state_dir,
    );

    let output = command(temp.path())
        .args(["steam", "accounts"])
        .output()
        .expect("run cluster-ctl");

    assert!(!output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains("Checking Steam accounts from"), "{stdout}");
    assert!(stdout.contains("alice"), "{stdout}");
    assert!(stdout.contains("bob"), "{stdout}");
    assert!(stdout.contains("carol"), "{stdout}");
    assert!(stdout.contains("UNKNOWN"), "{stdout}");
    assert!(!stdout.contains("not leased"), "{stdout}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("steam accounts: 3 VM(s) are not GUI-valid"),
        "{stderr}"
    );
}

#[test]
#[serial]
fn clean_logins_without_lease_uses_host_config_accounts() {
    let temp = TempDir::new().expect("tempdir");
    let login_state_dir = temp.path().join("logins");
    for vm in ["vm-1", "vm-2", "vm-3"] {
        let vm_dir = login_state_dir.join(vm);
        fs::create_dir_all(&vm_dir).expect("vm dir");
        fs::write(vm_dir.join("session.vdf"), "dummy").expect("session");
    }
    let host_path = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json(
        &host_path,
        3,
        &[("vm-1", "alice"), ("vm-2", "bob"), ("vm-3", "carol")],
        &login_state_dir,
    );

    let output = command(temp.path())
        .args(["steam", "clean-logins"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains("vm-1: cleaned"), "{stdout}");
    assert!(stdout.contains("vm-2: cleaned"), "{stdout}");
    assert!(stdout.contains("vm-3: cleaned"), "{stdout}");
    assert!(!login_state_dir.join("vm-1/session.vdf").exists());
    assert!(!login_state_dir.join("vm-2/session.vdf").exists());
    assert!(!login_state_dir.join("vm-3/session.vdf").exists());
}

#[test]
#[serial]
fn vm_count_flag_overrides_host_config() {
    let temp = TempDir::new().expect("tempdir");
    let login_state_dir = temp.path().join("logins");
    for vm in ["vm-1", "vm-2", "vm-3", "vm-4", "vm-5", "vm-6", "vm-7"] {
        let vm_dir = login_state_dir.join(vm);
        fs::create_dir_all(&vm_dir).expect("vm dir");
        fs::write(vm_dir.join("session.vdf"), "dummy").expect("session");
    }
    let lock_dir = temp.path().join("xdg-runtime/steampipe");
    write_claims(&lock_dir, "default", &[1, 2, 3, 4, 5, 6, 7]);
    let host_path = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json(
        &host_path,
        7,
        &[
            ("vm-1", "alice"),
            ("vm-2", "bob"),
            ("vm-3", "carol"),
            ("vm-4", "dave"),
            ("vm-5", "erin"),
            ("vm-6", "frank"),
            ("vm-7", "grace"),
        ],
        &login_state_dir,
    );

    let output = command(temp.path())
        .args(["--vm-count", "3", "steam", "clean-logins"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(stdout.contains("vm-1: cleaned"), "{stdout}");
    assert!(stdout.contains("vm-2: cleaned"), "{stdout}");
    assert!(stdout.contains("vm-3: cleaned"), "{stdout}");
    assert!(!stdout.contains("vm-4: cleaned"), "{stdout}");
    assert!(!stdout.contains("vm-7: cleaned"), "{stdout}");
    assert!(login_state_dir.join("vm-4/session.vdf").exists());
    assert!(login_state_dir.join("vm-5/session.vdf").exists());
    assert!(login_state_dir.join("vm-6/session.vdf").exists());
    assert!(login_state_dir.join("vm-7/session.vdf").exists());
}

#[test]
#[serial]
fn project_root_present_shadows_standalone_path() {
    let temp = TempDir::new().expect("tempdir");
    write_project_root(temp.path());
    let login_state_dir = temp.path().join("logins");
    fs::create_dir_all(&login_state_dir).expect("login state dir");
    let host_path = temp.path().join("xdg-config/steampipe/host.json");
    write_host_json(
        &host_path,
        2,
        &[("vm-1", "alice"), ("vm-2", "bob")],
        &login_state_dir,
    );

    let output = command(temp.path())
        .args(["steam", "info"])
        .output()
        .expect("run cluster-ctl");

    assert!(output.status.success(), "{output:?}");
    let stdout = output_string(&output);
    assert!(
        stdout.contains("Leased cluster vms (cluster=default):"),
        "{stdout}"
    );
    assert!(
        !stdout.contains("standalone Steam mode requires a discovered host config"),
        "{stdout}"
    );
}
