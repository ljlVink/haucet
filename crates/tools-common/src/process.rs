use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub fn hide_command_window(command: &mut Command) {
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
}
