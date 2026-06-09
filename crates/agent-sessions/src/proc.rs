use std::process::Command;

/// Suppress the console window that would otherwise flash when spawning a CLI
/// (e.g. `opencode session list`) on Windows. No-op everywhere else.
#[cfg(windows)]
pub fn hide_console(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
#[inline]
pub fn hide_console(_cmd: &mut Command) {}
