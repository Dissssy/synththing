//! Installing the desktop app on Windows: the same exe, moved to a folder of
//! its own (`%LOCALAPPDATA%\Programs\synththing`, the usual place for an app
//! installed for one user), with what a loose exe can't have: a Start menu
//! entry, an entry in Settings > Apps to uninstall it from, and starting
//! with Windows. Nothing changes for updates: the folder is the user's own,
//! so the installed exe swaps itself out in place (`updater.rs`) as any other.
//!
//! Only the app's own files go: songs, settings and soundfonts stay in the
//! config folder (`config::config_dir`), for a reinstall to find.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;

/// The uninstall entry, under HKEY_CURRENT_USER.
const UNINSTALL_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\synththing";
/// Programs started when the user signs in, under HKEY_CURRENT_USER.
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "synththing";
/// The argument starting with Windows gives, for `cli::Cli::autostart`.
pub const AUTOSTART_ARG: &str = "--autostart";

fn base_dirs() -> Result<BaseDirs> {
    BaseDirs::new().context("could not find this user's folders")
}

/// Where it's installed.
pub fn dir() -> Result<PathBuf> {
    Ok(base_dirs()?.data_local_dir().join("Programs").join("synththing"))
}

/// The installed exe.
pub fn exe() -> Result<PathBuf> {
    Ok(dir()?.join("synththing.exe"))
}

/// The Start menu entry (in the user's own Start menu folder).
fn shortcut() -> Result<PathBuf> {
    Ok(base_dirs()?.data_dir().join(r"Microsoft\Windows\Start Menu\Programs\synththing.lnk"))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// This is the installed exe running.
pub fn is_installed() -> bool {
    match (std::env::current_exe(), exe()) {
        (Ok(me), Ok(installed)) => same_file(&me, &installed),
        _ => false,
    }
}

/// This exe can be installed: not already, not a branch build (which lives
/// in a folder of its own, `channels.rs`), and not a test run with its own
/// config folder (whose Start menu entry would open the real one).
pub fn can_install() -> bool {
    !is_installed()
        && !matches!(crate::build_info::channel(), crate::build_info::Channel::Branch(_))
        && crate::config::config_dir_override().is_none()
}

/// Copy this exe into the install folder and add the Start menu entry and
/// the uninstall entry. Returns the installed exe, for the caller to start
/// in place of this one.
pub fn install() -> Result<PathBuf> {
    let me = std::env::current_exe().context("finding this program")?;
    let dir = dir()?;
    fs::create_dir_all(&dir).with_context(|| format!("making {}", dir.display()))?;
    let exe = dir.join("synththing.exe");
    fs::copy(&me, &exe).with_context(|| format!("copying synththing to {}", exe.display()))?;
    make_shortcut(&exe).context("adding synththing to the Start menu")?;
    register(&exe).context("adding synththing to Settings > Apps")?;
    log::info!("installed in {}", dir.display());
    Ok(exe)
}

/// Run by the installed exe on each launch: its uninstall entry keeps up
/// with updates (its version and size).
pub fn refresh() {
    if !is_installed() {
        return;
    }
    if let Err(e) = exe().and_then(|exe| register(&exe)) {
        log::warn!("couldn't update the uninstall entry: {e:#}");
    }
}

/// Write the uninstall entry Settings > Apps lists.
fn register(exe: &Path) -> Result<()> {
    let key = windows_registry::CURRENT_USER.create(UNINSTALL_KEY)?;
    let exe_text = exe.to_string_lossy();
    key.set_string("DisplayName", "synththing")?;
    key.set_string("DisplayVersion", env!("CARGO_PKG_VERSION"))?;
    key.set_string("Publisher", "synththing")?;
    key.set_string("DisplayIcon", &*exe_text)?;
    key.set_string("InstallLocation", &*exe.parent().unwrap_or(exe).to_string_lossy())?;
    key.set_string("UninstallString", format!("\"{exe_text}\" uninstall"))?;
    key.set_string("URLInfoAbout", "https://synththing.p51.nl")?;
    key.set_u32("NoModify", 1)?;
    key.set_u32("NoRepair", 1)?;
    if let Ok(meta) = fs::metadata(exe) {
        key.set_u32("EstimatedSize", u32::try_from(meta.len() / 1024).unwrap_or(u32::MAX))?;
    }
    Ok(())
}

/// The Start menu shortcut to `exe`, through the shell's own ShellLink.
fn make_shortcut(exe: &Path) -> Result<()> {
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
    };
    use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
    use windows::core::{HSTRING, Interface};

    let path = shortcut()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("making {}", parent.display()))?;
    }
    // (Already set up on the window's thread, by the window library: then
    // this only says so.)
    let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(exe.as_os_str()))?;
        if let Some(dir) = exe.parent() {
            link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
        }
        link.SetIconLocation(&HSTRING::from(exe.as_os_str()), 0)?;
        link.SetDescription(&HSTRING::from("Play MIDI files and audio, with visualizers"))?;
        link.cast::<IPersistFile>()?.Save(&HSTRING::from(path.as_os_str()), true)?;
    }
    Ok(())
}

/// Whether synththing starts when the user signs in to Windows.
pub fn starts_with_windows() -> bool {
    windows_registry::CURRENT_USER
        .open(RUN_KEY)
        .and_then(|key| key.get_string(RUN_VALUE))
        .is_ok()
}

/// Start (or stop starting) the installed exe when the user signs in.
pub fn set_starts_with_windows(on: bool) -> Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if on {
        key.set_string(RUN_VALUE, format!("\"{}\" {AUTOSTART_ARG}", exe()?.display()))?;
    } else if starts_with_windows() {
        key.remove_value(RUN_VALUE)?;
    }
    Ok(())
}

/// Take it all away again: the entries, then the install folder and this
/// exe in it (gone once this process ends). Call from the installed exe,
/// and quit straight after.
pub fn uninstall() -> Result<()> {
    if !is_installed() {
        bail!("this isn't the installed synththing");
    }
    set_starts_with_windows(false).context("no longer starting with Windows")?;
    let shortcut = shortcut()?;
    if shortcut.exists() {
        fs::remove_file(&shortcut).with_context(|| format!("removing {}", shortcut.display()))?;
    }
    if windows_registry::CURRENT_USER.open(UNINSTALL_KEY).is_ok() {
        windows_registry::CURRENT_USER.remove_tree(UNINSTALL_KEY).context("removing it from Settings > Apps")?;
    }
    let dir = dir()?;
    self_replace::self_delete_outside_path(&dir).context("removing synththing")?;
    let _ = fs::remove_dir_all(&dir);
    log::info!("uninstalled from {}", dir.display());
    Ok(())
}

/// `synththing uninstall`, which Settings > Apps runs: asks first, then
/// uninstalls, telling the user how it went (there's no terminal to say it
/// in). Quits instead when the app is running, after bringing it forward:
/// it can't be removed while it runs.
pub fn uninstall_command() -> i32 {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

    let _guard = match crate::instance::claim() {
        Ok(crate::instance::Claim::First(guard)) => guard,
        Ok(crate::instance::Claim::Running) => {
            MessageDialog::new()
                .set_level(MessageLevel::Info)
                .set_title("synththing")
                .set_description("synththing is running. Quit it first (synththing > Quit), then uninstall it again.")
                .show();
            return 1;
        }
        Err(e) => {
            log::warn!("couldn't check for synththing running: {e:#}");
            return 1;
        }
    };
    let data = crate::config::config_dir().map(|d| d.display().to_string()).unwrap_or_default();
    let answer = MessageDialog::new()
        .set_level(MessageLevel::Info)
        .set_title("Uninstall synththing")
        .set_description(format!(
            "Remove synththing from this computer? Your songs, settings and soundfonts stay in {data}."
        ))
        .set_buttons(MessageButtons::YesNo)
        .show();
    if answer != MessageDialogResult::Yes {
        return 1;
    }
    match uninstall() {
        Ok(()) => 0,
        Err(e) => {
            MessageDialog::new()
                .set_level(MessageLevel::Error)
                .set_title("synththing")
                .set_description(format!("Couldn't uninstall synththing: {e:#}"))
                .show();
            1
        }
    }
}
