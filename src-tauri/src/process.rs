//! On Windows, every subprocess a GUI app spawns flashes a console window
//! unless CREATE_NO_WINDOW is set, so all spawns go through here.

use std::ffi::OsStr;
#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;
use std::process::Command;

#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x08000000;

pub fn new_command(program: impl AsRef<OsStr>) -> Command {
    let cmd = Command::new(program);
    #[cfg(target_os = "windows")]
    let cmd = {
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    };
    cmd
}

/// Same guarantee as `new_command`, for tokio-managed children.
pub fn new_async_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    let cmd = tokio::process::Command::new(program);
    #[cfg(target_os = "windows")]
    let cmd = {
        let mut cmd = cmd;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    };
    cmd
}
