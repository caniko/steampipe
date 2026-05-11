use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

/// Per-VM Steam credentials and game key, as defined in the TOML file.
#[derive(Debug, Clone, Deserialize)]
pub struct VmCredentials {
    pub steam_user: String,
    pub steam_pass: String,
    pub game_key: Option<String>,
}

/// Top-level TOML structure.
///
/// ```toml
/// [vm.vm-1]
/// steam_user = "account1"
/// steam_pass = "password1"
/// game_key   = "XXXXX-XXXXX-XXXXX"
///
/// [vm.vm-2]
/// steam_user = "account2"
/// steam_pass = "password2"
/// game_key   = "YYYYY-YYYYY-YYYYY"
/// ```
#[derive(Debug, Deserialize)]
struct CredentialsFile {
    vm: HashMap<String, VmCredentials>,
}

/// Parsed credentials keyed by VM name (e.g. "vm-1").
pub type CredentialsMap = HashMap<String, VmCredentials>;

/// Load and parse a credentials file.
///
/// If the path ends in `.age`, decrypts it first using `rage -d` (or `age -d`).
/// Decryption uses the identity file if provided, otherwise falls back to
/// ssh-agent (which supports hardware keys like Nitrokey).
pub fn load(path: &Path, identity: Option<&Path>) -> anyhow::Result<CredentialsMap> {
    let contents = if is_age_encrypted(path) {
        decrypt_age(path, identity)?
    } else {
        std::fs::read_to_string(path).map_err(|e| {
            anyhow::anyhow!("Failed to read credentials file {}: {e}", path.display())
        })?
    };

    let file: CredentialsFile = toml::from_str(&contents)
        .map_err(|e| anyhow::anyhow!("Failed to parse credentials file {}: {e}", path.display()))?;
    Ok(file.vm)
}

/// Check if a file is age-encrypted (by extension).
fn is_age_encrypted(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "age")
}

/// Decrypt an age-encrypted file, trying `rage` first, then `age`.
/// Uses ssh-agent by default (works with Nitrokey/hardware keys).
/// An explicit identity file can be passed for non-agent scenarios.
fn decrypt_age(path: &Path, identity: Option<&Path>) -> anyhow::Result<String> {
    // Try rage first (Rust implementation, common in Nix), fall back to age
    for bin in ["rage", "age"] {
        if let Ok(output) = run_age_decrypt(bin, path, identity) {
            return Ok(output);
        }
    }
    anyhow::bail!(
        "Failed to decrypt {}. Ensure `rage` or `age` is installed and your \
         key is available (via ssh-agent for hardware keys, or --age-identity).",
        path.display()
    )
}

fn run_age_decrypt(bin: &str, path: &Path, identity: Option<&Path>) -> anyhow::Result<String> {
    let mut cmd = std::process::Command::new(bin);
    cmd.arg("-d");

    if let Some(id) = identity {
        cmd.args(["-i", &id.display().to_string()]);
    }

    cmd.arg(path);

    let output = cmd
        .output()
        .map_err(|e| anyhow::anyhow!("{bin} not found or failed to execute: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("{bin} decryption failed: {stderr}");
    }

    String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("Decrypted output is not valid UTF-8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_age_encrypted_matches_age_extension() {
        assert!(is_age_encrypted(Path::new("secrets.toml.age")));
        assert!(is_age_encrypted(Path::new("/path/to/foo.age")));
    }

    #[test]
    fn is_age_encrypted_rejects_other_extensions() {
        assert!(!is_age_encrypted(Path::new("secrets.toml")));
        assert!(!is_age_encrypted(Path::new("foo")));
        assert!(!is_age_encrypted(Path::new("foo.age.bak")));
    }

    fn tmp_file(name: &str, content: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("steampipe-cred-test-{name}"));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn load_parses_plain_toml() {
        let path = tmp_file(
            "plain.toml",
            r#"
[vm.vm-1]
steam_user = "alice"
steam_pass = "pw1"
game_key = "AAA-BBB-CCC"

[vm.vm-2]
steam_user = "bob"
steam_pass = "pw2"
"#,
        );

        let creds = load(&path, None).unwrap();
        assert_eq!(creds.len(), 2);
        let vm1 = &creds["vm-1"];
        assert_eq!(vm1.steam_user, "alice");
        assert_eq!(vm1.steam_pass, "pw1");
        assert_eq!(vm1.game_key.as_deref(), Some("AAA-BBB-CCC"));
        let vm2 = &creds["vm-2"];
        assert_eq!(vm2.steam_user, "bob");
        assert!(vm2.game_key.is_none());

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_errors_on_invalid_toml() {
        let path = tmp_file("invalid.toml", "this is = not valid =? toml [[[");
        let err = load(&path, None).unwrap_err();
        assert!(err.to_string().contains("Failed to parse credentials file"));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_errors_on_missing_file() {
        let path = std::env::temp_dir().join("steampipe-cred-test-does-not-exist.toml");
        let _ = std::fs::remove_file(&path);
        let err = load(&path, None).unwrap_err();
        assert!(err.to_string().contains("Failed to read credentials file"));
    }
}
