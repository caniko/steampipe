//! Write a minted Steam session to a VM's login-state share.
//!
//! Given a [`MintedSession`], this writes the on-disk artifacts under
//! `loginStateDir/<vm>/config/` that (a) make the host detector
//! ([`crate::game::accounts::classify_session_status`]) report `Ok`, and (b)
//! provide the refresh token the VM's Steam client would auto-login from:
//!
//! - `config/loginusers.vdf` — the account block (SteamID64 + AccountName +
//!   PersonaName + RememberPassword/AllowAutoLogin/MostRecent + Timestamp).
//! - `config/<steamID64>/local.vdf` — the SteamClient-audience refresh-token JWT.
//!
//! Writes are atomic (temp file + rename) and the token file lands before
//! `loginusers.vdf`, so a partial write never leaves a half-valid session.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::game::steam_auth::MintedSession;

/// Write `session` for `vm` under `login_state_dir`, replacing any prior session.
pub(crate) fn write_session(
    login_state_dir: &Path,
    vm: &str,
    session: &MintedSession,
) -> anyhow::Result<()> {
    let steam_id = session.steam_id64.to_string();
    let config_dir = login_state_dir.join(vm).join("config");
    let steam_id_dir = config_dir.join(&steam_id);
    std::fs::create_dir_all(&steam_id_dir)
        .with_context(|| format!("creating {}", steam_id_dir.display()))?;

    // Token file first: the session only "appears" complete once loginusers.vdf
    // (written second) references a SteamID whose token is already present.
    let local_vdf = steam_id_dir.join("local.vdf");
    write_atomic(
        &local_vdf,
        &render_local_vdf(&steam_id, &session.refresh_token),
    )?;

    let persona = session
        .persona_name
        .as_deref()
        .unwrap_or(&session.account_name);
    let loginusers = config_dir.join("loginusers.vdf");
    write_atomic(
        &loginusers,
        &render_loginusers_vdf(&steam_id, &session.account_name, persona),
    )?;

    Ok(())
}

fn render_loginusers_vdf(steam_id64: &str, account_name: &str, persona_name: &str) -> String {
    // Two-space-indented Valve VDF; the detector greps for the 17-digit SteamID
    // key and "PersonaName". RememberPassword/AllowAutoLogin/MostRecent are what
    // the real client needs to auto-login without prompting.
    format!(
        "\"users\"\n{{\n\t\"{id}\"\n\t{{\n\t\t\"AccountName\"\t\t\"{acct}\"\n\t\t\"PersonaName\"\t\t\"{persona}\"\n\t\t\"RememberPassword\"\t\t\"1\"\n\t\t\"WantsOfflineMode\"\t\t\"0\"\n\t\t\"SkipOfflineModeWarning\"\t\t\"0\"\n\t\t\"AllowAutoLogin\"\t\t\"1\"\n\t\t\"MostRecent\"\t\t\"1\"\n\t}}\n}}\n",
        id = steam_id64,
        acct = vdf_escape(account_name),
        persona = vdf_escape(persona_name),
    )
}

fn render_local_vdf(steam_id64: &str, refresh_token: &str) -> String {
    // Mirror the modern client's per-SteamID store nesting so the GUI can find
    // the refresh token; the host detector only requires the quoted "eyJ..."
    // value to be present in this file.
    format!(
        "\"MachineUserConfigStore\"\n{{\n\t\"Software\"\n\t{{\n\t\t\"Valve\"\n\t\t{{\n\t\t\t\"Steam\"\n\t\t\t{{\n\t\t\t\t\"ConnectCache\"\n\t\t\t\t{{\n\t\t\t\t\t\"{id}\"\t\t\"{token}\"\n\t\t\t\t}}\n\t\t\t}}\n\t\t}}\n\t}}\n}}\n",
        id = steam_id64,
        token = vdf_escape(refresh_token),
    )
}

/// Escape a VDF string value (`\` and `"`).
fn vdf_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Write `contents` to `path` atomically (temp file in the same dir + rename),
/// with owner-only permissions.
fn write_atomic(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .context("artifact path has no parent directory")?;
    let tmp = tmp_sibling(path);
    std::fs::write(&tmp, contents).with_context(|| format!("writing {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))?;
    // Best-effort: nothing else to flush for a config file.
    let _ = dir;
    Ok(())
}

fn tmp_sibling(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "session".into());
    name.push_str(".tmp");
    path.with_file_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::accounts::{self, SessionStatus};

    fn jwt_with_exp(exp: i64) -> String {
        use base64::Engine as _;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"aud":["web","client"],"exp":{exp}}}"#));
        format!("eyJhbGciOiJub25lIn0.{payload}.sig")
    }

    fn sample(steam_id64: u64, exp: i64) -> MintedSession {
        MintedSession {
            steam_id64,
            account_name: "caniko_jarvis_1".into(),
            persona_name: None,
            refresh_token: jwt_with_exp(exp),
            expires_at: exp,
        }
    }

    #[test]
    fn written_session_is_classified_ok() {
        let dir = tempfile::tempdir().unwrap();
        let steam_id = 76_561_198_727_542_502u64;
        let future = chrono::Utc::now().timestamp() + 200 * 86_400;
        write_session(dir.path(), "vm-1", &sample(steam_id, future)).unwrap();

        // Detector accepts the minted artifacts.
        let status = accounts::classify_session_status(
            dir.path(),
            "vm-1",
            accounts::DEFAULT_WARN_WITHIN_DAYS,
        );
        assert_eq!(status, SessionStatus::Ok, "minted session must classify Ok");

        // Files exist where expected.
        let config = dir.path().join("vm-1").join("config");
        assert!(config.join("loginusers.vdf").is_file());
        assert!(
            config
                .join(steam_id.to_string())
                .join("local.vdf")
                .is_file()
        );

        // loginusers.vdf carries the 17-digit SteamID + a PersonaName.
        let lu = std::fs::read_to_string(config.join("loginusers.vdf")).unwrap();
        assert!(lu.contains(&steam_id.to_string()));
        assert!(lu.contains("\"PersonaName\""));
        // local.vdf carries the eyJ token.
        let lv =
            std::fs::read_to_string(config.join(steam_id.to_string()).join("local.vdf")).unwrap();
        assert!(lv.contains("eyJ"));
    }

    #[test]
    fn rewrite_replaces_prior_session() {
        let dir = tempfile::tempdir().unwrap();
        let future = chrono::Utc::now().timestamp() + 200 * 86_400;
        write_session(dir.path(), "vm-2", &sample(76_561_198_000_000_001, future)).unwrap();
        // Re-mint (e.g. --force) overwrites cleanly without leaving .tmp files.
        write_session(dir.path(), "vm-2", &sample(76_561_198_000_000_001, future)).unwrap();
        let config = dir.path().join("vm-2").join("config");
        let stray: Vec<_> = std::fs::read_dir(&config)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(stray.is_empty(), "no leftover .tmp files");
    }

    #[test]
    fn persona_defaults_to_account_name() {
        let dir = tempfile::tempdir().unwrap();
        let future = chrono::Utc::now().timestamp() + 200 * 86_400;
        write_session(dir.path(), "vm-3", &sample(76_561_198_000_000_002, future)).unwrap();
        let lu = std::fs::read_to_string(
            dir.path()
                .join("vm-3")
                .join("config")
                .join("loginusers.vdf"),
        )
        .unwrap();
        assert!(lu.contains("\"PersonaName\"\t\t\"caniko_jarvis_1\""));
    }
}
