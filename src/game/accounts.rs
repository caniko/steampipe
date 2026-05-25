use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::UNIX_EPOCH;

use base64::Engine;

use crate::core::config::ClusterConfig;
use crate::core::nixos_module::NixosModuleConfig;

/// Per-VM account info collected from the host login state directory.
#[derive(Debug)]
struct AccountInfo {
    vm_name: String,
    account: Option<String>,
    persona: Option<String>,
    steam_id: Option<String>,
    logged_in: bool,
    refresh_age_days: Option<i64>,
    expires_in_days: Option<i64>,
    status: &'static str,
}

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

    if config.vms.is_empty() {
        println!(
            "==> Configured Steam accounts ({}; not leased)\n",
            host.accounts.len()
        );
        println!("{:<8} {:<12} {:<20}", "VM", "Status", "Account");
        println!("{}", "-".repeat(44));
        for account in &host.accounts {
            println!(
                "{:<8} {:<12} {:<20}",
                account.vm, "not leased", account.steam_user
            );
        }
        if host.accounts.is_empty() {
            println!();
            println!("  No configured accounts in host config");
        }
        return Ok(());
    }

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

    let mut logged_in = 0;
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
            info.status
        );
        if info.logged_in {
            logged_in += 1;
        }
    }

    println!();
    if logged_in == total {
        println!("  All {total} VMs have active Steam sessions");
    } else {
        let missing = total - logged_in;
        println!("  {logged_in}/{total} VMs logged in ({missing} need login)");
        if missing > 0 {
            println!("  Run `cluster-ctl steam login` to log in missing VMs");
        }
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
            "steam accounts: refresh token expiry is within {warn_within_days} day(s): {summary}"
        );
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

fn read_account_info(
    login_state_dir: &Path,
    vm_name: String,
    account: Option<String>,
    warn_within_days: i64,
) -> AccountInfo {
    let login_dir = login_state_dir.join(&vm_name);
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
            status: "NO_TOKEN",
        };
    };

    let Some((token_path, jwt)) = find_refresh_token_file(&login_dir, steam_id_for_token) else {
        return AccountInfo {
            vm_name,
            account,
            persona,
            steam_id,
            logged_in,
            refresh_age_days: None,
            expires_in_days: None,
            status: "NO_TOKEN",
        };
    };

    let now = chrono::Utc::now().timestamp();
    let refresh_age_days = modified_unix(&token_path).map(|mtime| (now - mtime) / 86_400);
    let expires_in_days = jwt_exp_unix(&jwt).map(|exp| (exp - now) / 86_400);
    let status = match expires_in_days {
        Some(days) if days < 0 => "EXPIRED",
        Some(days) if days < warn_within_days => "STALE",
        Some(_) => "OK",
        None => "NO_TOKEN",
    };

    AccountInfo {
        vm_name,
        account,
        persona,
        steam_id,
        logged_in,
        refresh_age_days,
        expires_in_days,
        status,
    }
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

fn find_refresh_token_file(login_dir: &Path, steam_id: &str) -> Option<(PathBuf, String)> {
    let local_vdf = login_dir.join("config").join(steam_id).join("local.vdf");
    if let Some(jwt) = read_refresh_jwt(&local_vdf) {
        return Some((local_vdf, jwt));
    }

    let config_vdf = login_dir.join("config").join("config.vdf");
    read_refresh_jwt(&config_vdf).map(|jwt| (config_vdf, jwt))
}

fn read_refresh_jwt(path: &Path) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    extract_refresh_jwt(&contents).map(ToOwned::to_owned)
}

fn extract_refresh_jwt(contents: &str) -> Option<&str> {
    static RE: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r#""(eyJ[A-Za-z0-9_\-\.]+)""#).unwrap());
    RE.captures(contents)
        .and_then(|captures| captures.get(1))
        .map(|match_| match_.as_str())
}

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

    #[test]
    fn extract_refresh_jwt_picks_jwt_shaped_string() {
        let vdf = r#""RefreshToken_76561198000000001"   "eyJhbGciOiJSUzI1NiJ9.eyJleHAiOjE3OTUxNzgwMDB9.sig" "#;
        let jwt = extract_refresh_jwt(vdf).expect("jwt present");
        assert!(jwt.starts_with("eyJ"));
    }

    #[test]
    fn jwt_exp_unix_parses_payload() {
        let jwt = "eyJhbGciOiJub25lIn0.eyJleHAiOjE3OTUxNzgwMDB9.";
        assert_eq!(jwt_exp_unix(jwt), Some(1_795_178_000));
    }
}
