//! Headless SteamClient refresh-token mint.
//!
//! This replaces the in-VM steamcmd-bootstrap + Steam-GUI login engine for
//! `steam login`: it authenticates the SteamClient platform directly via Valve's
//! `IAuthentication` flow (using the vendored `steam-vent` fork) and returns a
//! [`MintedSession`] whose `refresh_token` carries the `client` audience the
//! VM's Steam client needs. No in-VM Steam GUI and no X server are involved.
//!
//! The one-time Steam Guard code is sourced through the existing
//! [`GuardProvider`], opened as a retry-capable [`GuardRetrySession`] (host iced
//! dialog / `--code` / stdin / fail-fast), so the flow stays headless- and
//! AI-compatible. The fork's [`Connection::login_with_retry`] begins
//! authentication once (mailing the email code once) and then loops
//! submit→poll: a rejected code is detected by a poll timeout and re-prompts the
//! *same* session via [`RetryConfirmationHandler`], so the dialog can correct a
//! mistyped code inline instead of restarting auth (which would mail a fresh
//! code).

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{Error as IoError, ErrorKind, Write as _};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Context as _;
use base64::Engine as _;
use tokio::sync::Mutex as AsyncMutex;
use zeroize::Zeroize as _;

use steam_vent::auth::{
    ConfirmationMethod, GuardDataStore, NullGuardDataStore, RetryConfirmationHandler,
};
use steam_vent::{Connection, ServerList};

use crate::core::credentials::VmCredentials;
use crate::game::steam::{GuardCodeNeeded, GuardProvider, GuardRetryNext, GuardRetrySession};

/// A Steam session minted headlessly, ready to be written to a VM's login-state
/// share by the artifact writer (Phase 03).
#[derive(Debug, Clone)]
pub(crate) struct MintedSession {
    pub steam_id64: u64,
    /// The login (account) name — i.e. `steam_user`.
    pub account_name: String,
    /// Display name if known; `None` falls back to `account_name` when writing
    /// `loginusers.vdf`.
    pub persona_name: Option<String>,
    /// The SteamClient-audience refresh token JWT (`eyJ...`).
    pub refresh_token: String,
    /// Unix expiry from the JWT `exp` claim (0 if undecodable — informational
    /// only; the detector re-decodes `exp` from the written token).
    pub expires_at: i64,
}

/// Outcome of a mint attempt: either a session, or a typed "needs a Guard code"
/// signal for the headless/fail-fast contract.
pub(crate) enum MintOutcome {
    Minted(MintedSession),
    GuardCodeNeeded(GuardCodeNeeded),
}

/// Why a retry session ended without yielding a usable token, so the mint can
/// map `login_with_retry`'s `Aborted` to the right outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbortReason {
    /// Operator cancelled / the prompt timed out → a plain error.
    Cancelled,
    /// No code channel (fail-fast / dialog unavailable) → `GuardCodeNeeded`.
    Unavailable,
}

/// How `mint_session` reports a `login_with_retry` failure, derived from why the
/// Guard prompt (if any) ended. Extracted as a pure function so the headless
/// contract is unit-testable without a live Steam connection: a deliberate
/// cancel must hard-fail, but a missing code channel must surface the typed
/// `GuardCodeNeeded` so an AI/headless caller can react.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbortOutcome {
    /// No code channel (fail-fast / dialog unavailable): emit `GuardCodeNeeded`.
    GuardCodeNeeded,
    /// The operator declined: a plain "cancelled" error.
    Cancelled,
    /// A genuine auth failure unrelated to the prompt: a redacted error.
    LoginFailed,
}

fn classify_abort(reason: Option<AbortReason>) -> AbortOutcome {
    match reason {
        Some(AbortReason::Unavailable) => AbortOutcome::GuardCodeNeeded,
        Some(AbortReason::Cancelled) => AbortOutcome::Cancelled,
        None => AbortOutcome::LoginFailed,
    }
}

/// Bridges a [`GuardRetrySession`] to the fork's [`RetryConfirmationHandler`].
///
/// The session is shared (`Arc<AsyncMutex>`) so the mint can close a successful
/// dialog *after* `login_with_retry` has consumed the handler. `abort_reason` is
/// recorded when the operator declines so the mint can distinguish a deliberate
/// cancel from a missing code channel.
struct DialogRetryHandler {
    session: Arc<AsyncMutex<GuardRetrySession>>,
    abort_reason: Arc<StdMutex<Option<AbortReason>>>,
}

impl RetryConfirmationHandler for DialogRetryHandler {
    fn next_code(
        &mut self,
        _allowed_confirmations: &[ConfirmationMethod],
        previous_error: Option<&str>,
    ) -> impl std::future::Future<Output = Option<String>> + Send {
        // Own the inputs so the returned future is independent of `&mut self`'s
        // borrow (and thus `'static + Send`).
        let previous_error = previous_error.map(str::to_owned);
        let session = Arc::clone(&self.session);
        let abort_reason = Arc::clone(&self.abort_reason);
        async move {
            let mut session = session.lock().await;
            match session.next_code(previous_error.as_deref()).await {
                GuardRetryNext::Code(code) => Some(code),
                GuardRetryNext::Cancelled => {
                    *abort_reason.lock().expect("abort-reason mutex") =
                        Some(AbortReason::Cancelled);
                    None
                }
                GuardRetryNext::Unavailable => {
                    *abort_reason.lock().expect("abort-reason mutex") =
                        Some(AbortReason::Unavailable);
                    None
                }
            }
        }
    }
}

enum SteampipeGuardDataStore {
    File { path: PathBuf },
    Null(NullGuardDataStore),
}

impl SteampipeGuardDataStore {
    fn new(path: Option<&Path>) -> Self {
        match path {
            Some(path) => Self::File {
                path: path.to_path_buf(),
            },
            None => Self::Null(NullGuardDataStore),
        }
    }

    fn load_all(path: &Path) -> Result<HashMap<String, String>, IoError> {
        match fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw).map_err(|err| {
                IoError::new(
                    ErrorKind::InvalidData,
                    format!(
                        "parse Steam Guard machine tokens at {}: {err}",
                        path.display()
                    ),
                )
            }),
            Err(err) if err.kind() == ErrorKind::NotFound => Ok(HashMap::new()),
            Err(err) => Err(err),
        }
    }

    fn save_all(path: &Path, tokens: &HashMap<String, String>) -> Result<(), IoError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        }

        let raw = serde_json::to_vec(tokens).map_err(|err| {
            IoError::new(
                ErrorKind::InvalidData,
                format!(
                    "encode Steam Guard machine tokens at {}: {err}",
                    path.display()
                ),
            )
        })?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut options = OpenOptions::new();
            options.create(true).write(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&tmp)?;
            file.write_all(&raw)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        #[cfg(unix)]
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, path)?;
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}

impl GuardDataStore for SteampipeGuardDataStore {
    type Err = IoError;

    async fn store(&mut self, account: &str, machine_token: String) -> Result<(), Self::Err> {
        match self {
            Self::File { path } => {
                if machine_token.is_empty() {
                    return Ok(());
                }
                let mut tokens = Self::load_all(path)?;
                tokens.insert(account.to_owned(), machine_token);
                Self::save_all(path, &tokens)
            }
            Self::Null(store) => store
                .store(account, machine_token)
                .await
                .map_err(|never| match never {}),
        }
    }

    async fn load(&mut self, account: &str) -> Result<Option<String>, Self::Err> {
        match self {
            Self::File { path } => Ok(Self::load_all(path)?
                .remove(account)
                .filter(|token| !token.is_empty())),
            Self::Null(store) => store.load(account).await.map_err(|never| match never {}),
        }
    }
}

/// Authenticate the SteamClient platform headlessly and return a minted session.
///
/// The Guard code (if Steam challenges) is obtained through `provider`, with a
/// rejected code re-prompted in-session. Returns `MintOutcome::GuardCodeNeeded`
/// when no code channel exists (headless fail-fast / dialog unavailable), and an
/// `Err` on a deliberate cancel or a genuine auth failure.
pub(crate) async fn mint_session(
    creds: &VmCredentials,
    vm_name: &str,
    login_reason: &str,
    provider: &GuardProvider,
    guard_data_path: Option<&Path>,
) -> anyhow::Result<MintOutcome> {
    let server_list = ServerList::discover()
        .await
        .context("Steam server discovery failed")?;

    let session = provider
        .open_retry_session(vm_name, &creds.steam_user, login_reason)
        .await;
    let session = Arc::new(AsyncMutex::new(session));
    let abort_reason = Arc::new(StdMutex::new(None));

    let handler = DialogRetryHandler {
        session: Arc::clone(&session),
        abort_reason: Arc::clone(&abort_reason),
    };

    let mut password = creds.steam_pass.clone();
    let login_result = Connection::login_with_retry(
        &server_list,
        &creds.steam_user,
        &password,
        SteampipeGuardDataStore::new(guard_data_path),
        handler,
    )
    .await;
    password.zeroize();

    let connection = match login_result {
        Ok(connection) => {
            // Close the dialog with success (exit 0) before reading the token.
            session.lock().await.confirm_success().await;
            connection
        }
        Err(err) => {
            // Map a declined / impossible Guard prompt to the right outcome; any
            // surviving dialog is reaped when `session` drops (kill_on_drop).
            let reason = *abort_reason.lock().expect("abort-reason mutex");
            return match classify_abort(reason) {
                AbortOutcome::GuardCodeNeeded => {
                    Ok(MintOutcome::GuardCodeNeeded(GuardCodeNeeded {
                        vm_name: vm_name.to_owned(),
                        steam_user: creds.steam_user.clone(),
                        reason: login_reason.to_owned(),
                    }))
                }
                AbortOutcome::Cancelled => anyhow::bail!(
                    "Steam login for {vm_name} was cancelled before a Steam Guard code was provided"
                ),
                AbortOutcome::LoginFailed => {
                    let msg = redact(&err.to_string(), &creds.steam_pass);
                    anyhow::bail!("Steam login failed for {vm_name}: {msg}");
                }
            };
        }
    };

    let refresh_token = connection
        .access_token()
        .map(str::to_owned)
        .context("Steam login returned no refresh token")?;
    let steam_id64 = u64::from(connection.steam_id());
    let expires_at = jwt_exp_unix(&refresh_token).unwrap_or(0);

    Ok(MintOutcome::Minted(MintedSession {
        steam_id64,
        account_name: creds.steam_user.clone(),
        persona_name: None,
        refresh_token,
        expires_at,
    }))
}

/// Decode the `exp` claim from a JWT's payload segment (base64url, no padding).
fn jwt_exp_unix(jwt: &str) -> Option<i64> {
    let payload_b64 = jwt.split('.').nth(1)?;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload_b64)
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    json.get("exp")?.as_i64()
}

fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_owned();
    }
    text.replace(secret, "[REDACTED]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jwt_exp_unix_decodes_exp() {
        // header.payload(exp=1795178000).sig, payload base64url-no-pad
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"aud":["web","client"],"exp":1795178000}"#);
        let jwt = format!("eyJhbGciOiJub25lIn0.{payload}.sig");
        assert_eq!(jwt_exp_unix(&jwt), Some(1_795_178_000));
    }

    #[test]
    fn jwt_exp_unix_handles_garbage() {
        assert_eq!(jwt_exp_unix("not-a-jwt"), None);
    }

    #[test]
    fn redact_replaces_secret() {
        assert_eq!(redact("pw=hunter2 here", "hunter2"), "pw=[REDACTED] here");
        assert_eq!(redact("anything", ""), "anything");
    }

    #[tokio::test]
    async fn steampipe_guard_data_store_persists_machine_tokens() {
        let temp = tempfile::tempdir().expect("temp dir");
        let path = temp.path().join("guard").join("machine_tokens.json");
        let mut store = SteampipeGuardDataStore::new(Some(&path));

        store
            .store("alice", "token-a".to_string())
            .await
            .expect("store alice");
        store
            .store("bob", "token-b".to_string())
            .await
            .expect("store bob");

        assert_eq!(
            store.load("alice").await.expect("load alice").as_deref(),
            Some("token-a")
        );
        assert_eq!(
            store.load("bob").await.expect("load bob").as_deref(),
            Some("token-b")
        );

        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
    }

    // The Cancelled-vs-Unavailable distinction is the headless contract: a
    // deliberate cancel hard-fails, but a missing code channel surfaces the typed
    // GuardCodeNeeded so an AI/headless caller re-routes. Pin both directions so
    // a refactor can't silently swap them.
    #[test]
    fn classify_abort_maps_unavailable_to_guard_code_needed() {
        assert_eq!(
            classify_abort(Some(AbortReason::Unavailable)),
            AbortOutcome::GuardCodeNeeded
        );
    }

    #[test]
    fn classify_abort_maps_cancelled_to_cancelled_error() {
        assert_eq!(
            classify_abort(Some(AbortReason::Cancelled)),
            AbortOutcome::Cancelled
        );
    }

    #[test]
    fn classify_abort_maps_no_reason_to_login_failed() {
        assert_eq!(classify_abort(None), AbortOutcome::LoginFailed);
    }

    fn write_fake_helper(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("helper");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    /// Drive a `DialogRetryHandler` over a real (fake-helper-backed) retry session
    /// and assert it records the right `AbortReason` and returns `None`. This
    /// covers the handler→`abort_reason` wiring that `classify_abort` then
    /// consumes; together they pin the full Aborted→outcome path without a live
    /// Steam connection.
    async fn handler_abort_reason_for(helper_body: &str) -> Option<AbortReason> {
        let temp = tempfile::tempdir().unwrap();
        let helper = write_fake_helper(temp.path(), helper_body);

        let session = crate::game::steam::GuardProvider::helper_gui_with_binary(helper)
            .open_retry_session("vm-1", "account", "test login")
            .await;
        let session = Arc::new(AsyncMutex::new(session));
        let abort_reason = Arc::new(StdMutex::new(None));
        let mut handler = DialogRetryHandler {
            session: Arc::clone(&session),
            abort_reason: Arc::clone(&abort_reason),
        };

        let code = handler.next_code(&[], None).await;
        assert!(code.is_none(), "a closed dialog must yield no code");
        *abort_reason.lock().unwrap()
    }

    #[tokio::test]
    async fn handler_records_cancelled_on_dialog_cancel() {
        // exit 10 == operator clicked Cancel.
        assert_eq!(
            handler_abort_reason_for("exit 10").await,
            Some(AbortReason::Cancelled)
        );
    }

    #[tokio::test]
    async fn handler_records_unavailable_on_display_failure() {
        // exit 12 == no usable display.
        assert_eq!(
            handler_abort_reason_for("exit 12").await,
            Some(AbortReason::Unavailable)
        );
    }
}
