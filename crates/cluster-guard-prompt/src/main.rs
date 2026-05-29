use std::env;
use std::io::{self, Write};
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::Parser;
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

    #[arg(long, default_value_t = DEFAULT_TIMEOUT_SECS)]
    timeout_secs: u64,
}

#[derive(Debug, Clone)]
enum Message {
    CodeChanged(String),
    Submit,
    Cancel,
    Timeout,
}

#[derive(Debug, Clone)]
enum Outcome {
    Cancelled,
    Timeout,
    Submitted(String),
}

#[derive(Debug)]
struct Prompt {
    vm: String,
    user: String,
    reason: String,
    invalid_retry: bool,
    timeout: Duration,
    code: String,
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
                        timeout,
                        code: String::new(),
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
        .window_size((420.0, 230.0))
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
            prompt.code = code;
            Task::none()
        }
        Message::Submit => {
            if prompt.code.trim().is_empty() {
                return Task::none();
            }

            set_outcome(
                &prompt.outcome,
                Outcome::Submitted(prompt.code.trim().to_owned()),
            );
            prompt.code.zeroize();
            close_window()
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
    }
}

fn view(prompt: &Prompt) -> Element<'_, Message> {
    let retry_text = if prompt.invalid_retry {
        text("The previous Steam Guard code was rejected. Enter a new code.").size(14)
    } else {
        text("Enter the current Steam Guard code.").size(14)
    };

    let content = column![
        text("Steam Guard").size(24),
        text(format!("VM: {}", prompt.vm)).size(14),
        text(format!("User: {}", prompt.user)).size(14),
        text(&prompt.reason).size(14),
        retry_text,
        text_input("Steam Guard code", &prompt.code)
            .secure(true)
            .on_input(Message::CodeChanged)
            .on_submit(Message::Submit)
            .padding(10)
            .size(18),
        row![
            button("Cancel").on_press(Message::Cancel).padding([8, 14]),
            button("Submit").on_press(Message::Submit).padding([8, 14]),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    ]
    .spacing(10)
    .align_x(Alignment::Start)
    .width(Length::Fill);

    container(content)
        .padding(20)
        .width(Length::Fill)
        .height(Length::Fill)
        .center_y(Length::Fill)
        .into()
}

fn title(_: &Prompt) -> String {
    String::from("Steam Guard")
}

fn theme(_: &Prompt) -> Theme {
    Theme::Dark
}

fn subscription(prompt: &Prompt) -> Subscription<Message> {
    iced::time::every(prompt.timeout).map(|_| Message::Timeout)
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
