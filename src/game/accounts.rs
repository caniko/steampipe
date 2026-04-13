use crate::core::backend::Backend;
use crate::core::config::{ClusterConfig, par_each_vm};

/// Per-VM account info collected from the instance.
#[derive(Debug)]
struct AccountInfo {
    vm_name: String,
    persona: Option<String>,
    steam_id: Option<String>,
    logged_in: bool,
}

/// Show Steam account status across all instances.
pub async fn show<S>(config: &ClusterConfig<S>) -> anyhow::Result<()> {
    let backend = config.backend.clone();

    println!(
        "==> Checking Steam accounts on {} VMs...\n",
        config.vms.len()
    );

    let mut results = par_each_vm(&config.vms, |vm| {
        let backend = backend.clone();
        async move {
            let info = check_account(&backend, &vm.ip).await;
            AccountInfo {
                vm_name: vm.name.to_string(),
                persona: info.0,
                steam_id: info.1,
                logged_in: info.2,
            }
        }
    })
    .await?;

    results.sort_by(|a, b| a.vm_name.cmp(&b.vm_name));

    println!(
        "{:<8} {:<6} {:<20} {:<20}",
        "VM", "Login", "Account", "SteamID"
    );
    println!("{}", "─".repeat(56));

    let mut logged_in = 0;
    let mut total = 0;
    for info in &results {
        total += 1;
        let login_str = if info.logged_in { "OK" } else { "─" };
        let persona = info.persona.as_deref().unwrap_or("─");
        let steam_id = info.steam_id.as_deref().unwrap_or("─");
        println!(
            "{:<8} {:<6} {:<20} {:<20}",
            info.vm_name, login_str, persona, steam_id
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
            println!("  Run `cluster-ctl steam-login` to log in missing VMs");
        }
    }

    // Check for duplicate accounts
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

    Ok(())
}

/// Read the local machine's Steam ID from loginusers.vdf.
pub fn local_steam_id() -> anyhow::Result<u64> {
    let home = std::env::var("HOME")?;
    let path = format!("{home}/.local/share/Steam/config/loginusers.vdf");
    let content = std::fs::read_to_string(&path)?;
    let re = regex::Regex::new(r#"^\s*"(\d{17})""#).unwrap();
    for line in content.lines() {
        if let Some(caps) = re.captures(line) {
            return caps[1].parse::<u64>().map_err(Into::into);
        }
    }
    anyhow::bail!("No Steam ID found in {path}")
}

async fn check_account(backend: &Backend, ip: &str) -> (Option<String>, Option<String>, bool) {
    let cmd = r#"
        LOGIN_FILE="$HOME/.local/share/Steam/config/loginusers.vdf"
        if [ -f "$LOGIN_FILE" ]; then
            PERSONA=$(grep -oP '"PersonaName"\s+"\K[^"]+' "$LOGIN_FILE" 2>/dev/null | head -1)
            STEAMID=$(grep -oP '^\s*"\K[0-9]{17}' "$LOGIN_FILE" 2>/dev/null | head -1)
            echo "LOGGED_IN:${PERSONA:-unknown}:${STEAMID:-unknown}"
        else
            echo "NOT_LOGGED_IN"
        fi
    "#;

    let result = backend.run_cmd(ip, cmd).await;
    let output = result.stdout.trim();

    if let Some(rest) = output.strip_prefix("LOGGED_IN:") {
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let persona = parts
            .first()
            .filter(|s| !s.is_empty() && **s != "unknown")
            .map(|s| s.to_string());
        let steam_id = parts
            .get(1)
            .filter(|s| !s.is_empty() && **s != "unknown")
            .map(|s| s.to_string());
        (persona, steam_id, true)
    } else {
        (None, None, false)
    }
}
