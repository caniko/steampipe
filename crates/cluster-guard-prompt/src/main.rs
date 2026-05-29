use std::env;
use std::io::{self, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
use iced::futures::{SinkExt, Stream};
use iced::widget::{button, column, container, row, text, text_input};
use iced::{window, Alignment, Element, Length, Subscription, Task, Theme};
use zeroize::Zeroize;

const EXIT_CANCELLED: i32 = 10;
const EXIT_TIMEOUT: i32 = 11;
const EXIT_DISPLAY_UNAVAILABLE: i32 = 12;
const DEFAULT_TIMEOUT_SECS: u64 = 120;

#[derive(Debug, Parser)]
#[command(about = "Prompt for a Steam Guard one-time code")]
struct Args {
    #[arg(long)]
    vm: String,

    #[arg(long)]
    user: String,

    #[arg(long)]
    reason: String,

    #[arg(long)]
    invalid_retry: bool,

    /// Persistent verify-retry mode: keep the window open after Submit, write the
    /// code to stdout, and await a control line on stdin — `OK` (login succeeded:
    /// close, exit 0) or `ERR <msg>` (rejected: show `<msg>`, re-enable input).
    /// Without this flag the dialog is one-shot (Submit prints the code and exits).
    #[arg(long)]
    interactive: bool,

    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
}

#[derive(Debug, Clone)]
enum Message {
    CodeChanged(String),
    Submit,
    Cancel,
    Timeout,
    /// A control line arrived from cluster-ctl on stdin (interactive mode).
    Control(Control),
    /// stdin closed (cluster-ctl went away) — treat as a cancel.
    StdinClosed,
}

/// A control line cluster-ctl writes to the helper's stdin in interactive mode.
#[derive(Debug, Clone)]
enum Control {
    /// `OK` — the submitted code logged in; close the window (exit 0).
    Accept,
    /// `ERR <msg>` — the code was rejected; show `<msg>` and re-enable input.
    Reject(String),
}

#[derive(Debug, Clone)]
enum Outcome {
    Cancelled,
    Timeout,
    /// One-shot mode: the code to print to stdout on exit.
    Submitted(String),
    /// Interactive mode: login already confirmed (code was streamed to stdout
    /// at Submit time); just exit 0.
    Accepted,
}

/// Interactive-mode UI phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Awaiting a code: input + Submit/Cancel enabled.
    Editing,
    /// A code was submitted; awaiting Steam's verdict from cluster-ctl.
    Verifying,
}

#[derive(Debug)]
struct Prompt {
    vm: String,
    user: String,
    reason: String,
    invalid_retry: bool,
    interactive: bool,
    timeout: Duration,
    code: String,
    phase: Phase,
    /// The most recent rejection message to surface inline (interactive mode).
    error: Option<String>,
    outcome: Arc<Mutex<Outcome>>,
}

fn main() {
    let args = Args::parse();

    if !has_display() {
        eprintln!(
            "cluster-guard-prompt: no usable host display (WAYLAND_DISPLAY/DISPLAY unset or socket missing); cannot show the Steam Guard dialog."
        );
        process::exit(EXIT_DISPLAY_UNAVAILABLE);
    }

    if let Some(error) = gui_runtime_error() {
        eprintln!("cluster-guard-prompt: {error}");
        process::exit(EXIT_DISPLAY_UNAVAILABLE);
    }

    let outcome = Arc::new(Mutex::new(Outcome::Cancelled));
    let app_outcome = Arc::clone(&outcome);
    let timeout = Duration::from_secs(args.timeout_secs);
    let vm = args.vm;
    let user = args.user;
    let reason = args.reason;
    let invalid_retry = args.invalid_retry;
    let interactive = args.interactive;

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|info| {
        // Surface a concise reason (no backtrace) so a failed dialog is
        // diagnosable instead of a silent exit-101. The code is never in scope
        // here, so this leaks no secret.
        eprintln!("cluster-guard-prompt: GUI backend panic: {info}");
    }));

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        iced::application(
            move || {
                (
                    Prompt {
                        vm: vm.clone(),
                        user: user.clone(),
                        reason: reason.clone(),
                        invalid_retry,
                        interactive,
                        timeout,
                        code: String::new(),
                        phase: Phase::Editing,
                        error: None,
                        outcome: Arc::clone(&app_outcome),
                    },
                    Task::none(),
                )
            },
            update,
            view,
        )
        .title(title)
        .theme(theme)
        .window_size((440.0, 360.0))
        .resizable(false)
        .centered()
        .subscription(subscription)
        .run()
    }));

    std::panic::set_hook(previous_hook);

    match result {
        Ok(Ok(())) => finish(outcome),
        Ok(Err(error)) => {
            eprintln!("cluster-guard-prompt: failed to start the GUI event loop: {error}");
            process::exit(EXIT_DISPLAY_UNAVAILABLE);
        }
        Err(_) => {
            eprintln!("cluster-guard-prompt: the GUI backend panicked while opening the window.");
            process::exit(EXIT_DISPLAY_UNAVAILABLE);
        }
    }
}

fn update(prompt: &mut Prompt, message: Message) -> Task<Message> {
    match message {
        Message::CodeChanged(code) => {
            // Ignore edits while a code is being verified (input is disabled).
            if prompt.phase == Phase::Editing {
                prompt.code = code;
            }
            Task::none()
        }
        Message::Submit => {
            if prompt.phase != Phase::Editing {
                return Task::none();
            }
            let mut code = prompt.code.trim().to_owned();
            if code.is_empty() {
                code.zeroize();
                return Task::none();
            }

            if prompt.interactive {
                // Stream the code to cluster-ctl now and keep the window open
                // until it reports OK / ERR. The window closes only on success.
                let written = write_code_line(&code);
                code.zeroize();
                prompt.code.zeroize();
                if written.is_err() {
                    // cluster-ctl's pipe is gone; nothing can consume the code.
                    set_outcome(&prompt.outcome, Outcome::Cancelled);
                    return close_window();
                }
                prompt.error = None;
                prompt.phase = Phase::Verifying;
                Task::none()
            } else {
                set_outcome(&prompt.outcome, Outcome::Submitted(code));
                prompt.code.zeroize();
                close_window()
            }
        }
        Message::Control(Control::Accept) => {
            prompt.code.zeroize();
            set_outcome(&prompt.outcome, Outcome::Accepted);
            close_window()
        }
        Message::Control(Control::Reject(reason)) => {
            // The submitted code was rejected: surface it and re-enable input.
            prompt.error = Some(reason);
            prompt.phase = Phase::Editing;
            Task::none()
        }
        Message::Cancel => {
            prompt.code.zeroize();
            set_outcome(&prompt.outcome, Outcome::Cancelled);
            close_window()
        }
        Message::Timeout => {
            prompt.code.zeroize();
            set_outcome(&prompt.outcome, Outcome::Timeout);
            close_window()
        }
        Message::StdinClosed => {
            prompt.code.zeroize();
            set_outcome(&prompt.outcome, Outcome::Cancelled);
            close_window()
        }
    }
}

/// Write a single code line to stdout and flush, so cluster-ctl's line reader
/// receives it immediately.
fn write_code_line(code: &str) -> io::Result<()> {
    let mut stdout = io::stdout();
    writeln!(stdout, "{code}")?;
    stdout.flush()
}

fn view(prompt: &Prompt) -> Element<'_, Message> {
    let header = column![
        text("Steam Guard").size(24),
        text(format!("VM: {}", prompt.vm)).size(14),
        text(format!("User: {}", prompt.user)).size(14),
        text(&prompt.reason).size(14),
    ]
    .spacing(12)
    .align_x(Alignment::Start)
    .width(Length::Fill);

    let body = if prompt.interactive && prompt.phase == Phase::Verifying {
        // A code is in flight: input disabled, awaiting Steam's verdict.
        column![text("Verifying the code with Steam…").size(14)]
            .spacing(12)
            .align_x(Alignment::Start)
            .width(Length::Fill)
    } else {
        // Prefer the live rejection message; fall back to the one-shot retry
        // banner, then the default prompt.
        let status_text = if let Some(error) = &prompt.error {
            text(format!("Rejected: {error}")).size(14)
        } else if prompt.invalid_retry {
            text("The previous Steam Guard code was rejected. Enter a new code.").size(14)
        } else {
            text("Enter the current Steam Guard code.").size(14)
        };

        column![
            status_text,
            // Generous vertical padding so the masked text / placeholder is not
            // clipped at the top and bottom of the input box.
            text_input("Steam Guard code", &prompt.code)
                .secure(true)
                .on_input(Message::CodeChanged)
                .on_submit(Message::Submit)
                .padding([12, 12])
                .size(18)
                .width(Length::Fill),
            row![
                button(text("Cancel").size(16))
                    .on_press(Message::Cancel)
                    .padding([10, 18]),
                button(text("Submit").size(16))
                    .on_press(Message::Submit)
                    .padding([10, 18]),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        ]
        .spacing(12)
        .align_x(Alignment::Start)
        .width(Length::Fill)
    };

    let content = column![header, body]
        .spacing(12)
        .align_x(Alignment::Start)
        .width(Length::Fill);

    // Top-aligned (not vertically centred): centring a column taller than the
    // window clipped the buttons off the bottom and squeezed the input.
    container(content)
        .padding(24)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
}

fn title(_: &Prompt) -> String {
    String::from("Steam Guard")
}

fn theme(_: &Prompt) -> Theme {
    Theme::Dark
}

fn subscription(prompt: &Prompt) -> Subscription<Message> {
    let timeout = iced::time::every(prompt.timeout).map(|_| Message::Timeout);
    if prompt.interactive {
        // Also listen for cluster-ctl's OK / ERR control lines on stdin.
        Subscription::batch([timeout, Subscription::run(control_stream)])
    } else {
        timeout
    }
}

/// Stream cluster-ctl's control lines (read from stdin) as [`Message`]s. Runs on
/// iced's tokio executor; ends after `OK` or when stdin closes.
fn control_stream() -> impl Stream<Item = Message> {
    iced::stream::channel(8, |mut sender: iced::futures::channel::mpsc::Sender<Message>| async move {
        use tokio::io::AsyncBufReadExt as _;

        let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    if let Some(message) = parse_control_line(&line) {
                        let stop = matches!(message, Message::Control(Control::Accept));
                        if sender.send(message).await.is_err() {
                            break;
                        }
                        if stop {
                            break;
                        }
                    }
                }
                Ok(None) | Err(_) => {
                    let _ = sender.send(Message::StdinClosed).await;
                    break;
                }
            }
        }
    })
}

/// Parse one control line. Recognises `OK` and `ERR <msg>`; unknown lines are
/// ignored (return `None`) so stray output can't drive the dialog.
fn parse_control_line(line: &str) -> Option<Message> {
    let line = line.trim();
    if line == "OK" {
        Some(Message::Control(Control::Accept))
    } else if let Some(rest) = line.strip_prefix("ERR ") {
        Some(Message::Control(Control::Reject(rest.trim().to_owned())))
    } else if line == "ERR" {
        Some(Message::Control(Control::Reject(
            "the Steam Guard code was rejected".to_owned(),
        )))
    } else {
        None
    }
}

fn finish(outcome: Arc<Mutex<Outcome>>) -> ! {
    let outcome = match outcome.lock() {
        Ok(mut guard) => std::mem::replace(&mut *guard, Outcome::Cancelled),
        Err(_) => Outcome::Cancelled,
    };

    match outcome {
        Outcome::Submitted(mut code) => {
            let write_result = writeln!(io::stdout(), "{code}");
            code.zeroize();

            if write_result.is_ok() {
                process::exit(0);
            }

            process::exit(1);
        }
        // Interactive success: the code was already streamed at Submit time.
        Outcome::Accepted => process::exit(0),
        Outcome::Timeout => process::exit(EXIT_TIMEOUT),
        Outcome::Cancelled => process::exit(EXIT_CANCELLED),
    }
}

fn set_outcome(outcome: &Mutex<Outcome>, value: Outcome) {
    if let Ok(mut guard) = outcome.lock() {
        *guard = value;
    }
}

fn close_window() -> Task<Message> {
    window::latest().then(|id| match id {
        Some(id) => window::close(id),
        None => Task::none(),
    })
}

fn has_display() -> bool {
    wayland_socket_available() || x11_socket_available()
}

fn gui_runtime_error() -> Option<String> {
    if !nixos_like_host() {
        return None;
    }

    let mut missing = Vec::new();

    if wayland_socket_available() {
        for (label, prefixes) in [
            ("Wayland client library", &["libwayland-client.so"][..]),
            ("keyboard support library", &["libxkbcommon.so"][..]),
        ] {
            if !library_path_contains(prefixes) {
                missing.push(label);
            }
        }
    }

    if x11_socket_available() && !library_path_contains(&["libX11.so"]) {
        missing.push("X11 client library");
    }

    if missing.is_empty() {
        return None;
    }

    Some(format!(
        "missing GUI runtime libraries on LD_LIBRARY_PATH ({}); run from the Nix dev shell or the packaged cluster-guard-prompt wrapper.",
        missing.join(", ")
    ))
}

fn nixos_like_host() -> bool {
    Path::new("/etc/NIXOS").exists()
        || env::var_os("NIX_PROFILES").is_some()
        || env::var_os("IN_NIX_SHELL").is_some()
}

fn library_path_contains(prefixes: &[&str]) -> bool {
    env::var_os("LD_LIBRARY_PATH")
        .map(|paths| {
            env::split_paths(&paths).any(|dir| {
                prefixes
                    .iter()
                    .any(|prefix| library_entry_exists(&dir, prefix))
            })
        })
        .unwrap_or(false)
}

fn library_entry_exists(dir: &Path, prefix: &str) -> bool {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries.filter_map(Result::ok).any(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name == prefix || name.starts_with(&format!("{prefix}.")))
            })
        })
        .unwrap_or(false)
}

fn wayland_socket_available() -> bool {
    let Some(display) = non_empty_env("WAYLAND_DISPLAY") else {
        return false;
    };

    let path = PathBuf::from(&display);

    if path.is_absolute() {
        return socket_path_exists(&path);
    }

    non_empty_env("XDG_RUNTIME_DIR")
        .map(|runtime_dir| socket_path_exists(Path::new(&runtime_dir).join(display)))
        .unwrap_or(false)
}

fn x11_socket_available() -> bool {
    let Some(display) = non_empty_env("DISPLAY") else {
        return false;
    };

    if let Some(display_number) = local_x11_display_number(&display) {
        return socket_path_exists(format!("/tmp/.X11-unix/X{display_number}"));
    }

    true
}

fn local_x11_display_number(display: &str) -> Option<&str> {
    let local = display
        .strip_prefix(':')
        .or_else(|| display.strip_prefix("unix:"))?;

    let display_number = local
        .split_once('.')
        .map(|(number, _)| number)
        .unwrap_or(local);

    if display_number.chars().all(|c| c.is_ascii_digit()) {
        Some(display_number)
    } else {
        None
    }
}

fn socket_path_exists(path: impl AsRef<Path>) -> bool {
    std::fs::metadata(path.as_ref())
        .map(|metadata| metadata.file_type().is_socket())
        .unwrap_or(false)
}

fn non_empty_env(key: &str) -> Option<String> {
    env::var(key).ok().filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_control_accepts_ok() {
        assert!(matches!(
            parse_control_line("OK"),
            Some(Message::Control(Control::Accept))
        ));
        // Surrounding whitespace is tolerated (the line reader may keep CR etc.).
        assert!(matches!(
            parse_control_line("  OK  "),
            Some(Message::Control(Control::Accept))
        ));
    }

    #[test]
    fn parse_control_extracts_error_message() {
        match parse_control_line("ERR the code was rejected") {
            Some(Message::Control(Control::Reject(msg))) => {
                assert_eq!(msg, "the code was rejected");
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn parse_control_bare_err_has_default_message() {
        match parse_control_line("ERR") {
            Some(Message::Control(Control::Reject(msg))) => assert!(!msg.is_empty()),
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn parse_control_ignores_unknown_lines() {
        assert!(parse_control_line("").is_none());
        assert!(parse_control_line("hello").is_none());
        assert!(parse_control_line("OKAY").is_none());
    }

    fn test_prompt(interactive: bool, code: &str) -> Prompt {
        Prompt {
            vm: "vm-1".to_owned(),
            user: "account".to_owned(),
            reason: "test login".to_owned(),
            invalid_retry: false,
            interactive,
            timeout: Duration::from_secs(120),
            code: code.to_owned(),
            phase: Phase::Editing,
            error: None,
            outcome: Arc::new(Mutex::new(Outcome::Cancelled)),
        }
    }

    fn outcome_of(prompt: &Prompt) -> Outcome {
        prompt.outcome.lock().unwrap().clone()
    }

    // `update()` is shared by one-shot and interactive modes; these pin both so a
    // change to the interactive branch can't silently regress the one-shot stdout
    // contract that cluster-ctl's `guard_code_from_helper_stdout` depends on.

    #[test]
    fn one_shot_submit_records_submitted_code() {
        let mut prompt = test_prompt(false, "ABCDE");
        let _ = update(&mut prompt, Message::Submit);
        assert!(matches!(outcome_of(&prompt), Outcome::Submitted(code) if code == "ABCDE"));
    }

    #[test]
    fn submit_with_blank_code_stays_editing() {
        let mut prompt = test_prompt(false, "   ");
        let _ = update(&mut prompt, Message::Submit);
        // No code was submitted; the default outcome and phase are untouched.
        assert!(matches!(outcome_of(&prompt), Outcome::Cancelled));
        assert_eq!(prompt.phase, Phase::Editing);
    }

    #[test]
    fn interactive_submit_enters_verifying() {
        let mut prompt = test_prompt(true, "ABCDE");
        let _ = update(&mut prompt, Message::Submit);
        assert_eq!(prompt.phase, Phase::Verifying);
        assert!(prompt.error.is_none());
    }

    #[test]
    fn reject_returns_to_editing_with_error() {
        let mut prompt = test_prompt(true, "");
        prompt.phase = Phase::Verifying;
        let _ = update(
            &mut prompt,
            Message::Control(Control::Reject("bad code".to_owned())),
        );
        assert_eq!(prompt.phase, Phase::Editing);
        assert_eq!(prompt.error.as_deref(), Some("bad code"));
    }

    #[test]
    fn accept_sets_accepted_outcome() {
        let mut prompt = test_prompt(true, "");
        prompt.phase = Phase::Verifying;
        let _ = update(&mut prompt, Message::Control(Control::Accept));
        assert!(matches!(outcome_of(&prompt), Outcome::Accepted));
    }
}
