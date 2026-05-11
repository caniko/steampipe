#![allow(dead_code)] // Phase F wires scene orchestration into user-facing flows.

pub mod action;
pub mod runner;
pub mod sync;

use std::future::Future;
use std::pin::Pin;

use crate::core::backend::{Backend, CmdOutput};

pub trait SceneBackend: Send + Sync {
    fn run_cmd<'a>(
        &'a self,
        ip: &'a str,
        cmd: &'a str,
    ) -> Pin<Box<dyn Future<Output = CmdOutput> + Send + 'a>>;
}

impl SceneBackend for Backend {
    fn run_cmd<'a>(
        &'a self,
        ip: &'a str,
        cmd: &'a str,
    ) -> Pin<Box<dyn Future<Output = CmdOutput> + Send + 'a>> {
        Box::pin(Backend::run_cmd(self, ip, cmd))
    }
}

pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

pub(crate) fn scene_env_prefix(vm_user: &str) -> String {
    format!(
        ". /etc/profile.d/steampipe-graphics.sh 2>/dev/null || true; \
         export XDG_RUNTIME_DIR=/tmp/runtime-{vm_user}; \
         export WAYLAND_DISPLAY=wayland-1"
    )
}
