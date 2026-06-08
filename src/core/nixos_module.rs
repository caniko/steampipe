//! Reader for the global host config — the JSON file that
//! describes a Steampipe cluster's shape for a single machine.
//! Discovered at, in precedence order:
//!   1. --host-config <path>
//!   2. $XDG_CONFIG_HOME/steampipe/host.json
//!   3. /etc/steampipe/host.json
//!   4. /etc/steampipe/module.json    (legacy NixOS-module path)
//!
//! See docs/src/configuration/host-config.md for the schema and
//! the user-facing walkthroughs.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Deserialize;

const DEFAULT_PATH: &str = "/etc/steampipe/module.json";
const LEGACY_SYSTEM_PATH: &str = DEFAULT_PATH;
const SYSTEM_PATH: &str = "/etc/steampipe/host.json";
const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1, 2, 3];
const LATEST_SCHEMA_VERSION: u32 = 3;
const MODULE_CONFIG_ENV: &str = "STEAMPIPE_MODULE_CONFIG";
const ETC_DIR_ENV: &str = "STEAMPIPE_ETC_DIR";
const USER_PATH_SUFFIX: &str = "steampipe/host.json";
const SYSTEM_PATH_SUFFIX: &str = "steampipe/host.json";
const LEGACY_SYSTEM_PATH_SUFFIX: &str = "steampipe/module.json";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryRow {
    pub label: String,
    pub path: Option<PathBuf>,
    pub status: RowStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowStatus {
    NotSet,
    NotFound,
    NoXdgHome,
    Selected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovery {
    pub rows: Vec<DiscoveryRow>,
    pub selected: Option<PathBuf>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct NixosModuleConfig {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "vmCount")]
    pub vm_count: u8,
    pub bridge: String,
    pub subnet: String,
    pub prefix: u8,
    #[serde(rename = "hostIp")]
    pub host_ip: String,
    #[serde(rename = "tapOwner")]
    pub tap_owner: Option<String>,
    #[serde(rename = "loginStateDir")]
    pub login_state_dir: PathBuf,
    #[serde(rename = "guardDataPath")]
    pub guard_data_path: Option<PathBuf>,
    #[serde(rename = "credentialsPath")]
    pub credentials_path: Option<PathBuf>,
    pub accounts: Vec<NixosModuleAccount>,
    #[serde(rename = "runnersDir")]
    pub runners_dir: Option<PathBuf>,
    #[serde(rename = "loginRunnersDir")]
    pub login_runners_dir: Option<PathBuf>,
    #[serde(rename = "sshKey")]
    pub ssh_key: Option<PathBuf>,
    #[serde(rename = "vmUser")]
    pub vm_user: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
pub struct NixosModuleAccount {
    pub vm: String,
    #[serde(rename = "steamUser")]
    pub steam_user: String,
}

pub fn load() -> anyhow::Result<Option<NixosModuleConfig>> {
    match std::env::var_os(MODULE_CONFIG_ENV) {
        Some(path) if !path.is_empty() => load_from(Path::new(&path)),
        _ => load_from(Path::new(DEFAULT_PATH)),
    }
}

#[allow(dead_code)]
pub fn discover_path(cli_override: Option<&Path>) -> Option<PathBuf> {
    discover_with_trace(cli_override).selected
}

#[allow(dead_code)]
pub fn load_from_any_default(
    cli_override: Option<&Path>,
) -> anyhow::Result<Option<NixosModuleConfig>> {
    let (_, loaded) = load_with_trace(cli_override)?;
    Ok(loaded)
}

pub fn load_with_trace(
    cli_override: Option<&Path>,
) -> anyhow::Result<(Discovery, Option<NixosModuleConfig>)> {
    let discovery = discover_with_trace(cli_override);
    let loaded = match discovery.selected.as_deref() {
        Some(path) => load_from(path),
        None => Ok(None),
    }?;
    Ok((discovery, loaded))
}

pub fn load_from(path: &Path) -> anyhow::Result<Option<NixosModuleConfig>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("read {}", path.display())),
    };

    let config: NixosModuleConfig =
        serde_json::from_slice(&bytes).with_context(|| format!("parse {}", path.display()))?;

    if !SUPPORTED_SCHEMA_VERSIONS.contains(&config.schema_version) {
        anyhow::bail!(
            "{} has schemaVersion {}, but cluster-ctl supports {:?} (see docs/src/configuration/host-config.md)",
            path.display(),
            config.schema_version,
            SUPPORTED_SCHEMA_VERSIONS
        );
    }

    if config.schema_version < LATEST_SCHEMA_VERSION {
        eprintln!(
            "warning: {} uses schemaVersion {}; upgrade to {} (the NixOS module emits the new version automatically after `nixos-rebuild`)",
            path.display(),
            config.schema_version,
            LATEST_SCHEMA_VERSION
        );
    }

    Ok(Some(config))
}

pub fn discover_with_trace(cli_override: Option<&Path>) -> Discovery {
    let system_paths = system_paths_from_env();
    let system_path_refs: Vec<&Path> = system_paths.iter().map(PathBuf::as_path).collect();
    discover_with_trace_with_roots(
        cli_override,
        xdg_config_home().as_deref(),
        &system_path_refs,
    )
}

fn discover_with_trace_with_roots(
    cli_override: Option<&Path>,
    xdg_config_root: Option<&Path>,
    system_paths: &[&Path],
) -> Discovery {
    let mut rows = Vec::with_capacity(4);
    let mut selected = None;

    if let Some(path) = cli_override {
        let path = path.to_path_buf();
        selected = Some(path.clone());
        rows.push(DiscoveryRow {
            label: "--host-config flag".into(),
            path: Some(path),
            status: RowStatus::Selected,
        });
    } else {
        rows.push(DiscoveryRow {
            label: "--host-config flag".into(),
            path: None,
            status: RowStatus::NotSet,
        });
    }

    if let Some(root) = xdg_config_root {
        let candidate = root.join(USER_PATH_SUFFIX);
        let status = if selected.is_none() && candidate.exists() {
            selected = Some(candidate.clone());
            RowStatus::Selected
        } else {
            RowStatus::NotFound
        };
        rows.push(DiscoveryRow {
            label: candidate.display().to_string(),
            path: Some(candidate),
            status,
        });
    } else {
        rows.push(DiscoveryRow {
            label: "XDG config home".into(),
            path: None,
            status: RowStatus::NoXdgHome,
        });
    }

    for path in system_paths {
        let candidate = path.to_path_buf();
        let status = if selected.is_none() && candidate.exists() {
            selected = Some(candidate.clone());
            RowStatus::Selected
        } else {
            RowStatus::NotFound
        };
        rows.push(DiscoveryRow {
            label: candidate.display().to_string(),
            path: Some(candidate),
            status,
        });
    }

    Discovery { rows, selected }
}

#[cfg(test)]
fn discover_path_with_roots(
    cli_override: Option<&Path>,
    xdg_config_root: Option<&Path>,
    system_paths: &[&Path],
) -> Option<PathBuf> {
    discover_with_trace_with_roots(cli_override, xdg_config_root, system_paths).selected
}

fn xdg_config_home() -> Option<PathBuf> {
    xdg_config_home_from_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

fn steampipe_etc_root() -> Option<PathBuf> {
    std::env::var_os(ETC_DIR_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn system_paths_from_env() -> [PathBuf; 2] {
    if let Some(root) = steampipe_etc_root() {
        [
            root.join(SYSTEM_PATH_SUFFIX),
            root.join(LEGACY_SYSTEM_PATH_SUFFIX),
        ]
    } else {
        [
            PathBuf::from(SYSTEM_PATH),
            PathBuf::from(LEGACY_SYSTEM_PATH),
        ]
    }
}

fn xdg_config_home_from_env(
    xdg_config_home: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    xdg_config_home
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })
}

#[cfg(test)]
mod tests {
    use super::{
        Discovery, RowStatus, discover_path_with_roots, discover_with_trace_with_roots, load_from,
        xdg_config_home_from_env,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    use serial_test::serial;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn load_from_missing_file_returns_none() {
        let missing = NamedTempFile::new().expect("temp file");
        let path = missing.path().to_path_buf();
        drop(missing);

        let loaded = load_from(&path).expect("missing file should not error");
        assert!(loaded.is_none());
    }

    #[test]
    fn load_from_parses_minimal_fixtures_for_supported_schema_versions() {
        for schema_version in [1, 2, 3] {
            let path = write_fixture(&format!(
                r#"{{"accounts":[],"bridge":"br-cluster","credentialsPath":null,"hostIp":"10.0.100.254","loginStateDir":"/var/lib/steampipe/logins","prefix":24,"schemaVersion":{schema_version},"subnet":"10.0.100","tapOwner":null,"vmCount":7}}"#,
            ));

            let loaded = load_from(&path)
                .expect("minimal fixture should parse")
                .expect("fixture should exist");

            assert_eq!(loaded.schema_version, schema_version);
            assert_eq!(loaded.vm_count, 7);
            assert_eq!(loaded.bridge, "br-cluster");
            assert_eq!(loaded.subnet, "10.0.100");
            assert_eq!(loaded.prefix, 24);
            assert_eq!(loaded.host_ip, "10.0.100.254");
            assert_eq!(loaded.tap_owner, None);
            assert_eq!(
                loaded.login_state_dir,
                PathBuf::from("/var/lib/steampipe/logins")
            );
            assert_eq!(loaded.guard_data_path, None);
            assert_eq!(loaded.credentials_path, None);
            assert!(loaded.accounts.is_empty());
            assert_eq!(loaded.runners_dir, None);
            assert_eq!(loaded.login_runners_dir, None);
            assert_eq!(loaded.ssh_key, None);
            assert_eq!(loaded.vm_user, None);
        }
    }

    #[test]
    fn load_from_parses_full_fixtures_for_supported_schema_versions() {
        for schema_version in [1, 2, 3] {
            let path = write_fixture(&format!(
                r#"{{"accounts":[{{"vm":"vm-1","steamUser":"testaccount1"}},{{"vm":"vm-2","steamUser":"testaccount2"}}],"bridge":"br-cluster","credentialsPath":"/etc/steampipe/credentials.toml","guardDataPath":"/var/lib/steampipe/guard/machine_tokens.json","hostIp":"10.0.100.254","loginRunnersDir":"/run/current-system/sw/share/steampipe/login-runners","loginStateDir":"/var/lib/steampipe/logins","prefix":24,"runnersDir":"/run/current-system/sw/share/steampipe/runners","schemaVersion":{schema_version},"sshKey":"/etc/steampipe/ssh/cluster_key","subnet":"10.0.100","tapOwner":"can","vmCount":7,"vmUser":"cluster"}}"#,
            ));

            let loaded = load_from(&path)
                .expect("full fixture should parse")
                .expect("fixture should exist");

            assert_eq!(loaded.schema_version, schema_version);
            assert_eq!(loaded.tap_owner.as_deref(), Some("can"));
            assert_eq!(
                loaded.credentials_path,
                Some(PathBuf::from("/etc/steampipe/credentials.toml"))
            );
            assert_eq!(
                loaded.guard_data_path,
                Some(PathBuf::from(
                    "/var/lib/steampipe/guard/machine_tokens.json"
                ))
            );
            assert_eq!(loaded.accounts.len(), 2);
            assert_eq!(loaded.accounts[0].vm, "vm-1");
            assert_eq!(loaded.accounts[0].steam_user, "testaccount1");
            assert_eq!(loaded.accounts[1].vm, "vm-2");
            assert_eq!(loaded.accounts[1].steam_user, "testaccount2");
            assert_eq!(
                loaded.runners_dir,
                Some(PathBuf::from(
                    "/run/current-system/sw/share/steampipe/runners"
                ))
            );
            assert_eq!(
                loaded.login_runners_dir,
                Some(PathBuf::from(
                    "/run/current-system/sw/share/steampipe/login-runners"
                ))
            );
            assert_eq!(
                loaded.ssh_key,
                Some(PathBuf::from("/etc/steampipe/ssh/cluster_key"))
            );
            assert_eq!(loaded.vm_user.as_deref(), Some("cluster"));
        }
    }

    #[test]
    fn load_from_ignores_unknown_fields() {
        let path = write_fixture(
            r#"{"accounts":[],"bridge":"br-cluster","credentialsPath":null,"futureField":"x","hostIp":"10.0.100.254","loginRunnersDir":null,"loginStateDir":"/var/lib/steampipe/logins","prefix":24,"schemaVersion":3,"subnet":"10.0.100","tapOwner":null,"vmCount":7}"#,
        );

        let loaded = load_from(&path)
            .expect("fixture with unknown field should parse")
            .expect("fixture should exist");

        assert_eq!(loaded.schema_version, 3);
        assert_eq!(loaded.vm_count, 7);
    }

    #[test]
    fn load_from_rejects_unknown_schema_version() {
        let path = write_fixture(
            r#"{"accounts":[],"bridge":"br-cluster","credentialsPath":null,"hostIp":"10.0.100.254","loginStateDir":"/var/lib/steampipe/logins","prefix":24,"schemaVersion":4,"subnet":"10.0.100","tapOwner":null,"vmCount":7}"#,
        );

        let error = load_from(&path).expect_err("unknown schema version should fail");
        let message = error.to_string();
        assert!(message.contains("schemaVersion 4"));
        assert!(message.contains("[1, 2, 3]"));
        assert!(message.contains("host-config.md"));
    }

    #[test]
    fn load_from_rejects_invalid_json() {
        let path = write_fixture("{not json");

        let error = load_from(&path).expect_err("invalid json should fail");
        assert!(error.to_string().contains("parse"));
    }

    #[test]
    #[serial]
    fn discover_path_prefers_cli_override_above_all() {
        let tempdir = TempDir::new().expect("temp dir");
        let cli_override = tempdir.path().join("explicit.json");
        let xdg_root = tempdir.path().join("xdg");
        let system_path = tempdir.path().join("etc-host.json");
        write_json(&xdg_root.join("steampipe/host.json"));
        write_json(&system_path);

        let discovered = discover_path_with_roots(
            Some(&cli_override),
            Some(&xdg_root),
            &[system_path.as_path()],
        );

        assert_eq!(discovered, Some(cli_override));
    }

    #[test]
    #[serial]
    fn discover_path_prefers_xdg_over_system() {
        let tempdir = TempDir::new().expect("temp dir");
        let xdg_root = tempdir.path().join("xdg");
        let xdg_host = xdg_root.join("steampipe/host.json");
        let system_host = tempdir.path().join("etc-host.json");
        write_json(&xdg_host);
        write_json(&system_host);

        let discovered = discover_path_with_roots(None, Some(&xdg_root), &[system_host.as_path()]);

        assert_eq!(discovered, Some(xdg_host));
    }

    #[test]
    #[serial]
    fn discover_path_falls_back_to_legacy_module_json_when_host_json_missing() {
        let tempdir = TempDir::new().expect("temp dir");
        let system_host = tempdir.path().join("etc-host.json");
        let legacy_system = tempdir.path().join("etc-module.json");
        write_json(&legacy_system);

        let discovered = discover_path_with_roots(
            None,
            Some(tempdir.path()),
            &[system_host.as_path(), legacy_system.as_path()],
        );

        assert_eq!(discovered, Some(legacy_system));
    }

    #[test]
    #[serial]
    fn discover_path_returns_none_when_nothing_present() {
        let tempdir = TempDir::new().expect("temp dir");
        let system_host = tempdir.path().join("etc-host.json");
        let legacy_system = tempdir.path().join("etc-module.json");

        let discovered = discover_path_with_roots(
            None,
            Some(tempdir.path()),
            &[system_host.as_path(), legacy_system.as_path()],
        );

        assert_eq!(discovered, None);
    }

    #[test]
    #[serial]
    fn discover_path_handles_empty_xdg_env_correctly() {
        let tempdir = TempDir::new().expect("temp dir");
        let home = tempdir.path().join("home");
        let expected = home.join(".config");
        fs::create_dir_all(&home).expect("create home");

        let discovered =
            xdg_config_home_from_env(Some("".into()), Some(home.clone().into_os_string()));

        assert_eq!(discovered, Some(expected));
    }

    #[test]
    fn discover_with_trace_marks_xdg_selection() {
        let tempdir = TempDir::new().expect("temp dir");
        let xdg_root = tempdir.path().join("xdg");
        let xdg_host = xdg_root.join("steampipe/host.json");
        write_json(&xdg_host);

        let discovery = discover_with_trace_with_roots(None, Some(&xdg_root), &[]);

        assert_eq!(discovery.selected, Some(xdg_host.clone()));
        assert_eq!(discovery.rows[0].status, RowStatus::NotSet);
        assert_eq!(discovery.rows[1].path, Some(xdg_host));
        assert_eq!(discovery.rows[1].status, RowStatus::Selected);
    }

    #[test]
    fn discover_with_trace_marks_no_xdg_home() {
        let discovery = discover_with_trace_with_roots(None, None, &[]);

        assert_eq!(
            discovery,
            Discovery {
                rows: vec![
                    super::DiscoveryRow {
                        label: "--host-config flag".into(),
                        path: None,
                        status: RowStatus::NotSet,
                    },
                    super::DiscoveryRow {
                        label: "XDG config home".into(),
                        path: None,
                        status: RowStatus::NoXdgHome,
                    },
                ],
                selected: None,
            }
        );
    }

    fn write_fixture(contents: &str) -> PathBuf {
        let file = NamedTempFile::new().expect("temp file");
        fs::write(file.path(), contents).expect("write fixture");
        file.into_temp_path().keep().expect("persist temp path")
    }

    fn write_json(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent dirs");
        }
        fs::write(path, "{}").expect("write json");
    }
}
