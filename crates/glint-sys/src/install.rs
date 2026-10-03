use anyhow::{Context, Result, ensure};
use glint_core::Settings;
use std::{
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
};
use windows::{
    Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND},
        System::{
            Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IPersistFile},
            Registry::*,
            Threading::CREATE_NO_WINDOW,
        },
        UI::Shell::{IShellLinkW, SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify, ShellLink},
    },
    core::{Interface, PCWSTR, w},
};

const RUN: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const UNINSTALL: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\Glint";
const KEYBOARD: &str = "Control Panel\\Keyboard";
const PRINT_SCREEN: &str = "PrintScreenKeyForSnippingEnabled";
const PREVIOUS: &str = "PrevPrintScreenKeyForSnippingEnabled";

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}
fn create_key(root: HKEY, path: &str) -> Result<Key> {
    let path = crate::util::wide(path);
    let mut key = HKEY::default();
    unsafe {
        RegCreateKeyExW(
            root,
            PCWSTR(path.as_ptr()),
            None,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &mut key,
            None,
        )
        .ok()?;
    }
    Ok(Key(key))
}
fn write_value(
    root: HKEY,
    path: &str,
    name: &str,
    kind: REG_VALUE_TYPE,
    bytes: &[u8],
) -> Result<()> {
    let key = create_key(root, path)?;
    let name = crate::util::wide(name);
    unsafe {
        RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, kind, Some(bytes)).ok()?;
    }
    Ok(())
}
fn write_string(root: HKEY, path: &str, name: &str, value: &str) -> Result<()> {
    let bytes: Vec<u8> = value
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    write_value(root, path, name, REG_SZ, &bytes)
}
fn write_dword(root: HKEY, path: &str, name: &str, value: u32) -> Result<()> {
    write_value(root, path, name, REG_DWORD, &value.to_le_bytes())
}
fn read_value(root: HKEY, path: &str, name: &str) -> Result<Option<(REG_VALUE_TYPE, Vec<u8>)>> {
    let path = crate::util::wide(path);
    let name = crate::util::wide(name);
    let mut kind = REG_VALUE_TYPE::default();
    let mut size = 0;
    let status = unsafe {
        RegGetValueW(
            root,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_ANY | RRF_NOEXPAND,
            Some(&mut kind),
            None,
            Some(&mut size),
        )
    };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    status.ok()?;
    let mut bytes = vec![0; size as usize];
    unsafe {
        RegGetValueW(
            root,
            PCWSTR(path.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_ANY | RRF_NOEXPAND,
            Some(&mut kind),
            Some(bytes.as_mut_ptr().cast()),
            Some(&mut size),
        )
        .ok()?;
    }
    bytes.truncate(size as usize);
    Ok(Some((kind, bytes)))
}
fn delete_value(root: HKEY, path: &str, name: &str) -> Result<()> {
    let path = crate::util::wide(path);
    let name = crate::util::wide(name);
    let mut key = HKEY::default();
    let status =
        unsafe { RegOpenKeyExW(root, PCWSTR(path.as_ptr()), None, KEY_SET_VALUE, &mut key) };
    if status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        return Ok(());
    }
    status.ok()?;
    let key = Key(key);
    let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
    if status != ERROR_FILE_NOT_FOUND {
        status.ok()?;
    }
    Ok(())
}
fn delete_tree(root: HKEY, path: &str) -> Result<()> {
    let path = crate::util::wide(path);
    let status = unsafe { RegDeleteTreeW(root, PCWSTR(path.as_ptr())) };
    if status != ERROR_FILE_NOT_FOUND && status != ERROR_PATH_NOT_FOUND {
        status.ok()?;
    }
    Ok(())
}

pub fn installed_exe_path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("Programs")
            .join("Glint")
            .join("glint.exe"),
    )
}
pub fn is_installed() -> bool {
    installed_exe_path().is_ok_and(|path| path.is_file())
}

fn exe_command(exe: &Path, args: &str) -> String {
    format!("\"{}\" {args}", exe.display())
}
fn shortcut_path() -> Result<PathBuf> {
    Ok(
        PathBuf::from(std::env::var_os("APPDATA").context("APPDATA is unavailable")?)
            .join("Microsoft/Windows/Start Menu/Programs/Glint.lnk"),
    )
}
pub fn set_launch_at_login(enabled: bool, exe: &Path) -> Result<()> {
    if enabled {
        write_string(
            HKEY_CURRENT_USER,
            RUN,
            "Glint",
            &exe_command(exe, "--background"),
        )
    } else {
        delete_value(HKEY_CURRENT_USER, RUN, "Glint")
    }
}

fn registration_strings(exe: &Path) -> Vec<(String, String, String)> {
    let location = exe.parent().unwrap_or(Path::new("")).display().to_string();
    let mut values = Vec::new();
    for (name, value) in [
        ("DisplayName", "Glint".into()),
        ("DisplayIcon", exe.display().to_string()),
        ("DisplayVersion", env!("CARGO_PKG_VERSION").into()),
        ("Publisher", "Glint".into()),
        ("InstallLocation", location),
        ("UninstallString", exe_command(exe, "--uninstall")),
    ] {
        values.push((UNINSTALL.into(), name.into(), value));
    }
    for (path, name, value) in [
        (
            "Software\\Classes\\Glint.ScreenClip",
            "",
            "Glint screen capture".into(),
        ),
        (
            "Software\\Classes\\Glint.ScreenClip",
            "URL Protocol",
            String::new(),
        ),
        (
            "Software\\Classes\\Glint.ScreenClip\\DefaultIcon",
            "",
            format!("\"{}\",0", exe.display()),
        ),
        (
            "Software\\Classes\\Glint.ScreenClip\\shell\\open\\command",
            "",
            exe_command(exe, "\"%1\""),
        ),
        (
            "Software\\Glint\\Capabilities",
            "ApplicationName",
            "Glint".into(),
        ),
        (
            "Software\\Glint\\Capabilities",
            "ApplicationDescription",
            "Glint screenshot and screen recording".into(),
        ),
        (
            "Software\\Glint\\Capabilities\\URLAssociations",
            "ms-screenclip",
            "Glint.ScreenClip".into(),
        ),
        (
            "Software\\RegisteredApplications",
            "Glint",
            "Software\\Glint\\Capabilities".into(),
        ),
    ] {
        values.push((path.into(), name.into(), value));
    }
    values
}

fn save_print_screen(root: HKEY) -> Result<()> {
    if read_value(root, "Software\\Glint", PREVIOUS)?.is_none() {
        let previous = match read_value(root, KEYBOARD, PRINT_SCREEN)? {
            None => "absent".to_string(),
            Some((kind, bytes)) => {
                ensure!(
                    kind == REG_DWORD && bytes.len() == 4,
                    "Unexpected Print Screen registry value type"
                );
                u32::from_le_bytes(bytes.try_into().expect("four bytes")).to_string()
            }
        };
        write_string(root, "Software\\Glint", PREVIOUS, &previous)?;
    }
    write_dword(root, KEYBOARD, PRINT_SCREEN, 0)
}
fn restore_print_screen(root: HKEY) -> Result<()> {
    if let Some((kind, bytes)) = read_value(root, "Software\\Glint", PREVIOUS)? {
        ensure!(kind == REG_SZ, "Unexpected saved Print Screen value type");
        let wide: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .take_while(|c| *c != 0)
            .collect();
        let value = String::from_utf16(&wide)?;
        if value == "absent" {
            delete_value(root, KEYBOARD, PRINT_SCREEN)?;
        } else {
            write_dword(
                root,
                KEYBOARD,
                PRINT_SCREEN,
                value.parse().context("Invalid saved Print Screen value")?,
            )?;
        }
        delete_value(root, "Software\\Glint", PREVIOUS)?;
    }
    Ok(())
}
fn create_shortcut(exe: &Path) -> Result<()> {
    let path = shortcut_path()?;
    std::fs::create_dir_all(path.parent().context("shortcut has no parent")?)?;
    let exe_wide = crate::util::wide(exe);
    let working = crate::util::wide(exe.parent().context("executable has no parent")?);
    let path_wide = crate::util::wide(&path);
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(exe_wide.as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(working.as_ptr()))?;
        link.SetDescription(w!("Glint screenshot and screen recording"))?;
        link.SetIconLocation(PCWSTR(exe_wide.as_ptr()), 0)?;
        let file: IPersistFile = link.cast()?;
        file.Save(PCWSTR(path_wide.as_ptr()), true)?;
    }
    Ok(())
}
fn notify_associations() {
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// Installs per-user. The caller must initialize COM before creating the shortcut.
pub fn install(current_exe: &Path, settings: &Settings) -> Result<PathBuf> {
    let target = installed_exe_path()?;
    std::fs::create_dir_all(target.parent().context("install path has no parent")?)?;
    let source = std::fs::canonicalize(current_exe)?;
    let same = std::fs::canonicalize(&target).is_ok_and(|path| path == source);
    if !same {
        std::fs::copy(&source, &target).with_context(|| format!("Could not copy Glint to {}. If the installed copy is running and locks this file, exit it and retry.", target.display()))?;
    }
    set_launch_at_login(settings.general.launch_at_login, &target)?;
    create_shortcut(&target)?;
    for (path, name, value) in registration_strings(&target) {
        write_string(HKEY_CURRENT_USER, &path, &name, &value)?;
    }
    for (name, value) in [
        ("NoModify", 1),
        ("NoRepair", 1),
        (
            "EstimatedSize",
            std::fs::metadata(&target)?
                .len()
                .div_ceil(1024)
                .min(u32::MAX as u64) as u32,
        ),
    ] {
        write_dword(HKEY_CURRENT_USER, UNINSTALL, name, value)?;
    }
    save_print_screen(HKEY_CURRENT_USER)?;
    notify_associations();
    Ok(target)
}

pub fn uninstall() -> Result<()> {
    let target = installed_exe_path()?;
    set_launch_at_login(false, &target)?;
    match std::fs::remove_file(shortcut_path()?) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    restore_print_screen(HKEY_CURRENT_USER)?;
    delete_tree(HKEY_CURRENT_USER, UNINSTALL)?;
    delete_tree(HKEY_CURRENT_USER, "Software\\Classes\\Glint.ScreenClip")?;
    delete_tree(HKEY_CURRENT_USER, "Software\\Glint\\Capabilities")?;
    delete_value(
        HKEY_CURRENT_USER,
        "Software\\RegisteredApplications",
        "Glint",
    )?;
    notify_associations();
    if !target.exists() {
        return Ok(());
    }
    let target = std::fs::canonicalize(target)?;
    let running = std::fs::canonicalize(std::env::current_exe()?)?;
    let install_dir = target.parent().context("install path has no parent")?;
    let expected_dir = std::fs::canonicalize(
        PathBuf::from(std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is unavailable")?)
            .join("Programs/Glint"),
    )?;
    ensure!(
        install_dir == expected_dir && target.file_name().is_some_and(|n| n == "glint.exe"),
        "Refusing to remove an unexpected install path"
    );
    if running == target {
        let system_root = std::env::var_os("SystemRoot").context("SystemRoot is unavailable")?;
        let command = r#"for /l %i in (1,1,30) do (ping -n 2 127.0.0.1 >nul & del /q "%GLINT_UNINSTALL_DIR%\glint.exe" 2>nul & if not exist "%GLINT_UNINSTALL_DIR%\glint.exe" (rd "%GLINT_UNINSTALL_DIR%" 2>nul & exit /b 0))"#;
        let directory = install_dir.to_string_lossy();
        let directory = directory.strip_prefix(r"\\?\").unwrap_or(&directory);
        std::process::Command::new(PathBuf::from(system_root).join("System32/cmd.exe"))
            .args(["/d", "/v:off", "/c"])
            .raw_arg(command)
            .env("GLINT_UNINSTALL_DIR", directory)
            .current_dir(
                expected_dir
                    .parent()
                    .context("install directory has no parent")?,
            )
            .creation_flags(CREATE_NO_WINDOW.0)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
    } else {
        std::fs::remove_file(&target)?;
        if let Err(error) = std::fs::remove_dir(install_dir)
            && error.kind() != std::io::ErrorKind::DirectoryNotEmpty
        {
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn open_default_apps_settings() -> Result<()> {
    crate::shell::open_uri("ms-settings:defaultapps?registeredAppUser=Glint")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quoted_commands_and_registration_values() {
        let exe = Path::new(r"C:\Users\User's Name\Programs\Glint\glint.exe");
        assert_eq!(
            exe_command(exe, "--background"),
            r#""C:\Users\User's Name\Programs\Glint\glint.exe" --background"#
        );
        let values = registration_strings(exe);
        assert!(
            values
                .iter()
                .any(|(path, name, value)| path.ends_with(r"shell\open\command")
                    && name.is_empty()
                    && value.ends_with(r#" "%1""#))
        );
        assert!(
            values
                .iter()
                .any(|(_, name, value)| name == "ms-screenclip" && value == "Glint.ScreenClip")
        );
        assert!(
            !values
                .iter()
                .any(|(path, _, _)| path.contains("UserChoice"))
        );
    }
}
