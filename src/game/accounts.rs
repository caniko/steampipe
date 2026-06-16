use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use base64::Engine;

use crate::core::config::ClusterConfig;
use crate::core::nixos_module::NixosModuleConfig;

/// Per-VM account info collected from the host login state directory.
#[derive(Debug)]
pub(crate) struct AccountInfo {
    pub(crate) vm_name: String,
    pub(crate) account: Option<String>,
    pub(crate) persona: Option<String>,
    pub(crate) steam_id: Option<String>,
    pub(crate) logged_in: bool,
    pub(crate) refresh_age_days: Option<i64>,
    pub(crate) expires_in_days: Option<i64>,
    pub(crate) status: SessionStatus,
}

/// Host-side Steam session status derived from persisted login state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(dead_code)]
pub(crate) enum SessionStatus {
    Ok,
    Stale,
    Expired,
    NoToken,
    Unknown,
}

impl SessionStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Stale => "STALE",
            Self::Expired => "EXPIRED",
            Self::NoToken => "NO_TOKEN",
            Self::Unknown => "UNKNOWN",
        }
    }

    pub(crate) fn login_reason(self) -> &'static str {
        match self {
            Self::Ok => "session is healthy",
            Self::Stale => "Steam GUI session cache is stale",
            Self::Expired => "Steam GUI session cache expired",
            Self::NoToken => "no GUI-valid Steam session found",
            Self::Unknown => "session status unknown",
        }
    }
}

pub(crate) const DEFAULT_WARN_WITHIN_DAYS: i64 = 30;

/// Show Steam account status across all instances.
pub async fn show<S>(
    config: &ClusterConfig<S>,
    host: Option<&NixosModuleConfig>,
    warn_within_days: i64,
) -> anyhow::Result<()> {
    let host = host.ok_or_else(|| {
        anyhow::anyhow!(
            "steam accounts: no loginStateDir configured; this command requires the steampipe NixOS module or --login-state-dir to be set."
        )
    })?;

    let targets = account_targets(config, host);
    if targets.is_empty() {
        println!(
            "==> Configured Steam accounts ({}; not leased)\n",
            host.accounts.len()
        );
        println!("{:<8} {:<12} {:<20}", "VM", "Status", "Account");
        println!("{}", "-".repeat(44));
        println!();
        println!("  No configured accounts in host config");
        return Ok(());
    }

    println!(
        "==> Checking Steam accounts from {}...\n",
        host.login_state_dir.display()
    );

    let mut results: Vec<AccountInfo> = targets
        .into_iter()
        .map(|(vm_name, account)| {
            read_account_info(&host.login_state_dir, vm_name, account, warn_within_days)
        })
        .collect();

    results.sort_by(|a, b| a.vm_name.cmp(&b.vm_name));

    println!(
        "{:<8} {:<6} {:<20} {:<20} {:>10} {:>10} {:<8}",
        "VM", "Login", "Account", "SteamID", "RefreshAge", "ExpiresIn", "Status"
    );
    println!("{}", "-".repeat(94));

    let mut gui_valid = 0;
    let mut total = 0;
    for info in &results {
        total += 1;
        let login_str = if info.logged_in { "OK" } else { "-" };
        let account = info
            .persona
            .as_deref()
            .or(info.account.as_deref())
            .unwrap_or("-");
        let steam_id = info.steam_id.as_deref().unwrap_or("-");
        println!(
            "{:<8} {:<6} {:<20} {:<20} {:>10} {:>10} {:<8}",
            info.vm_name,
            login_str,
            account,
            steam_id,
            format_days(info.refresh_age_days),
            format_days(info.expires_in_days),
            info.status.as_str()
        );
        if info.status == SessionStatus::Ok {
            gui_valid += 1;
        }
    }

    println!();
    let missing_gui_valid = total - gui_valid;
    if missing_gui_valid == 0 {
        println!("  All {total} VMs have GUI-valid Steam sessions");
    } else {
        println!(
            "  {gui_valid}/{total} VMs have GUI-valid Steam sessions ({missing_gui_valid} need login)"
        );
        println!("  Run `cluster-ctl steam login <vm> --force` for each VM that is not GUI-valid");
    }

    // Check for duplicate accounts.
    let personas: Vec<&str> = results
        .iter()
        .filter_map(|r| r.persona.as_deref())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut dupes = Vec::new();
    for p in &personas {
        if !seen.insert(*p) {
            dupes.push(*p);
        }
    }
    if !dupes.is_empty() {
        println!();
        println!(
            "  WARNING: Duplicate accounts detected: {}",
            dupes.join(", ")
        );
        println!("  Steam may have issues with multiple sessions on the same account");
    }

    let stale: Vec<&AccountInfo> = results
        .iter()
        .filter(|info| matches!(info.expires_in_days, Some(days) if days < warn_within_days))
        .collect();
    if !stale.is_empty() {
        let summary = stale
            .iter()
            .map(|info| {
                format!(
                    "{} expires in {}",
                    info.vm_name,
                    format_days(info.expires_in_days)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        anyhow::bail!(
            "steam accounts: Steam GUI session cache age is within {warn_within_days} day(s): {summary}"
        );
    }

    if missing_gui_valid > 0 {
        anyhow::bail!("steam accounts: {missing_gui_valid} VM(s) are not GUI-valid");
    }

    Ok(())
}

/// Read the local machine's Steam ID from loginusers.vdf.
pub fn local_steam_id() -> anyhow::Result<u64> {
    let home = std::env::var("HOME")?;
    let path = format!("{home}/.local/share/Steam/config/loginusers.vdf");
    let content = std::fs::read_to_string(&path)?;
    parse_steam_id_from_loginusers(&content)
        .and_then(|id| id.parse::<u64>().ok())
        .ok_or_else(|| anyhow::anyhow!("No Steam ID found in {path}"))
}

fn account_targets<S>(
    config: &ClusterConfig<S>,
    host: &NixosModuleConfig,
) -> Vec<(String, Option<String>)> {
    if config.vms.is_empty() {
        return host
            .accounts
            .iter()
            .map(|account| (account.vm.clone(), Some(account.steam_user.clone())))
            .collect();
    }

    config
        .vms
        .iter()
        .map(|vm| {
            let vm_name = vm.name.to_string();
            let account = host
                .accounts
                .iter()
                .find(|account| account.vm == vm_name)
                .map(|account| account.steam_user.clone());
            (vm_name, account)
        })
        .collect()
}

pub(crate) fn read_account_info(
    login_state_dir: &Path,
    vm_name: String,
    account: Option<String>,
    warn_within_days: i64,
) -> AccountInfo {
    let login_dir = login_state_dir.join(&vm_name);
    if !login_dir.is_dir() {
        return AccountInfo {
            vm_name,
            account,
            persona: None,
            steam_id: None,
            logged_in: false,
            refresh_age_days: None,
            expires_in_days: None,
            status: SessionStatus::Unknown,
        };
    }

    let loginusers = login_dir.join("config").join("loginusers.vdf");
    let (persona, steam_id) = parse_loginusers_file(&loginusers).unwrap_or((None, None));
    let logged_in = steam_id.is_some();

    let Some(steam_id_for_token) = steam_id.as_deref() else {
        return AccountInfo {
            vm_name,
            account,
            persona,
            steam_id,
            logged_in,
            refresh_age_days: None,
            expires_in_days: None,
            status: SessionStatus::NoToken,
        };
    };

    let Some(cache_path) = find_gui_session_cache_file(&login_dir, steam_id_for_token) else {
        return AccountInfo {
            vm_name,
            account,
            persona,
            steam_id,
            logged_in,
            refresh_age_days: None,
            expires_in_days: None,
            status: SessionStatus::NoToken,
        };
    };

    let now = chrono::Utc::now().timestamp();
    let refresh_age_days = modified_unix(&cache_path).map(|mtime| (now - mtime) / 86_400);
    let _ = warn_within_days;

    AccountInfo {
        vm_name,
        account,
        persona,
        steam_id,
        logged_in,
        refresh_age_days,
        expires_in_days: None,
        status: SessionStatus::Ok,
    }
}

pub(crate) fn classify_session_status(
    login_state_dir: &Path,
    vm_name: &str,
    warn_within_days: i64,
) -> SessionStatus {
    read_account_info(login_state_dir, vm_name.to_owned(), None, warn_within_days).status
}

fn parse_loginusers_file(path: &Path) -> Option<(Option<String>, Option<String>)> {
    let content = std::fs::read_to_string(path).ok()?;
    let persona = parse_persona_from_loginusers(&content);
    let steam_id = parse_steam_id_from_loginusers(&content);
    Some((persona, steam_id))
}

fn parse_persona_from_loginusers(contents: &str) -> Option<String> {
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""PersonaName"\s+"([^"]+)""#).unwrap());
    RE.captures(contents)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str().to_string())
}

fn parse_steam_id_from_loginusers(contents: &str) -> Option<String> {
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#"(?m)^\s*"(\d{17})""#).unwrap());
    RE.captures(contents)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str().to_string())
}

#[allow(dead_code)]
fn find_refresh_token_file(login_dir: &Path, steam_id: &str) -> Option<(PathBuf, String)> {
    let local_vdf = login_dir.join("config").join(steam_id).join("local.vdf");
    if let Some(jwt) = read_refresh_jwt(&local_vdf) {
        return Some((local_vdf, jwt));
    }

    let config_vdf = login_dir.join("config").join("config.vdf");
    read_refresh_jwt(&config_vdf).map(|jwt| (config_vdf, jwt))
}

fn find_gui_session_cache_file(login_dir: &Path, steam_id: &str) -> Option<PathBuf> {
    let candidates = [
        login_dir.join("local.vdf"),
        login_dir.join("config").join("config.vdf"),
        login_dir.join("config").join(steam_id).join("local.vdf"),
    ];
    candidates
        .into_iter()
        .find(|path| contains_gui_connect_cache(path))
}

fn contains_gui_connect_cache(path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    if !contents.contains("\"ConnectCache\"") {
        return false;
    }
    extract_connect_cache_values(&contents)
        .into_iter()
        .any(is_gui_cache_value)
}

fn extract_connect_cache_values(contents: &str) -> Vec<&str> {
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""[^"]+"\s+"([^"]+)""#).unwrap());
    RE.captures_iter(contents)
        .filter_map(|captures| captures.get(1).map(|match_| match_.as_str()))
        .collect()
}

fn is_gui_cache_value(value: &str) -> bool {
    !value.starts_with("ey")
        && value.len() >= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-' || byte == b'_')
}

#[allow(dead_code)]
fn read_refresh_jwt(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    extract_refresh_jwt(&contents).map(ToOwned::to_owned)
}

#[allow(dead_code)]
fn extract_refresh_jwt(contents: &str) -> Option<&str> {
    // Steam's refresh tokens are JWTs whose header/payload JSON is space-padded
    // (`{ "typ": "JWT", ... }`), so they base64url-encode to an `eyA...` prefix
    // rather than the canonical `eyJ...`. Accept any `ey`-prefixed base64url JWT
    // (dots included) so both real Steam tokens and canonical JWTs are detected;
    // a non-JWT false match is harmless because its payload won't decode an
    // `exp` claim and is classified `NoToken`.
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""(ey[A-Za-z0-9_\-\.]+)""#).unwrap());
    RE.captures(contents)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str())
}

#[allow(dead_code)]
fn jwt_exp_unix(jwt: &str) -> Option<i64> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload_bytes).ok()?;
    json.get("exp")?.as_i64()
}

fn modified_unix(path: &Path) -> Option<i64> {
    std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() as i64)
}

fn format_days(days: Option<i64>) -> String {
    match days {
        Some(days) => format!("{days}d"),
        None => "-".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_jwt(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        let payload =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!(r#"{{"exp":{exp}}}"#));
        format!("{header}.{payload}.")
    }

    fn write_loginusers(config_dir: &Path, persona: &str, steam_id: &str) {
        std::fs::create_dir_all(config_dir).expect("create config dir");
        std::fs::write(
            config_dir.join("loginusers.vdf"),
            format!(
                r#""users"
{{
    "{steam_id}"
    {{
        "PersonaName"    "{persona}"
    }}
}}
"#
            ),
        )
        .expect("write loginusers.vdf");
    }

    fn write_vm_state(root: &Path, vm_name: &str, persona: &str, steam_id: &str, exp: i64) {
        let config_dir = root.join(vm_name).join("config");
        let token_dir = config_dir.join(steam_id);
        std::fs::create_dir_all(&token_dir).expect("create token dir");
        write_loginusers(&config_dir, persona, steam_id);
        std::fs::write(
            token_dir.join("local.vdf"),
            format!(r#""RefreshToken_{steam_id}"   "{}""#, test_jwt(exp)),
        )
        .expect("write local.vdf");
    }

    fn write_gui_vm_state(root: &Path, vm_name: &str, persona: &str, steam_id: &str) {
        let login_dir = root.join(vm_name);
        let config_dir = login_dir.join("config");
        std::fs::create_dir_all(&config_dir).expect("create config dir");
        write_loginusers(&config_dir, persona, steam_id);
        std::fs::write(
            login_dir.join("local.vdf"),
            r#""MachineUserConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "ConnectCache"
                {
                    "ea8626751" "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                }
            }
        }
    }
}
"#,
        )
        .expect("write local.vdf");
    }

    #[test]
    fn extract_refresh_jwt_picks_jwt_shaped_string() {
        let vdf = r#""RefreshToken_76561198000000001"   "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjE3OTUxNzgwMDB9.sig" "#;
        let jwt = extract_refresh_jwt(vdf).expect("jwt present");
        assert!(jwt.starts_with("eyJ"));
    }

    #[test]
    fn extract_refresh_jwt_matches_space_padded_steam_token() {
        // Real Steam client refresh tokens have a space-padded JSON header
        // (`{ "typ"...`), encoding to an `eyA...` prefix — must still be detected.
        let vdf = r#""76561198727542502"   "eyAidHlwIjogIkpXVCIgfQ.eyAiZXhwIjogMTc5ODMzNTQxMiB9.sig-part_AB-cd" "#;
        let jwt = extract_refresh_jwt(vdf).expect("steam jwt present");
        assert!(jwt.starts_with("eyA"));
        assert_eq!(jwt.split('.').count(), 3);
    }

    #[test]
    fn jwt_exp_unix_parses_payload() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjE3OTUxNzgwMDB9.";
        assert_eq!(jwt_exp_unix(jwt), Some(1_795_178_000));
    }

    #[test]
    fn classify_session_status_handles_each_state() {
        let temp = tempfile::tempdir().expect("temp login state dir");
        write_gui_vm_state(temp.path(), "vm-ok", "PersonaOk", "76561198000000001");
        std::fs::create_dir_all(temp.path().join("vm-no-token").join("config"))
            .expect("create no-token dir");

        assert_eq!(
            classify_session_status(temp.path(), "vm-ok", DEFAULT_WARN_WITHIN_DAYS),
            SessionStatus::Ok
        );
        assert_eq!(
            classify_session_status(temp.path(), "vm-no-token", DEFAULT_WARN_WITHIN_DAYS),
            SessionStatus::NoToken
        );
        assert_eq!(
            classify_session_status(temp.path(), "vm-missing", DEFAULT_WARN_WITHIN_DAYS),
            SessionStatus::Unknown
        );
    }

    #[test]
    fn classify_session_status_rejects_jwt_only_artifact() {
        let temp = tempfile::tempdir().expect("temp login state dir");
        let now = chrono::Utc::now().timestamp();
        write_vm_state(
            temp.path(),
            "vm-jwt-only",
            "PersonaJwt",
            "76561198000000004",
            now + 90 * 86_400,
        );

        assert_eq!(
            classify_session_status(temp.path(), "vm-jwt-only", DEFAULT_WARN_WITHIN_DAYS),
            SessionStatus::NoToken
        );
    }

    #[test]
    fn classify_session_status_accepts_gui_config_vdf_cache() {
        let temp = tempfile::tempdir().expect("temp login state dir");
        let steam_id = "76561198000000005";
        let config_dir = temp.path().join("vm-config-only").join("config");
        write_loginusers(&config_dir, "PersonaConfig", steam_id);
        std::fs::write(
            config_dir.join("config.vdf"),
            r#""InstallConfigStore"
{
    "Software"
    {
        "Valve"
        {
            "Steam"
            {
                "ConnectCache"
                {
                    "ea8626751" "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                }
            }
        }
    }
}
"#,
        )
        .expect("write config.vdf");

        assert_eq!(
            classify_session_status(temp.path(), "vm-config-only", DEFAULT_WARN_WITHIN_DAYS),
            SessionStatus::Ok
        );
    }
}
