//! Starting with Windows: bliss's entry among the programs Windows starts when
//! the user signs in.
//!
//! The entry is the value `bliss` under the user's Run key: this executable's
//! path in quotes, and `--minimized` for a window that opens minimized. A Run
//! value rather than the scheduled task the plan first named: the window needs
//! no elevation — stage 2 moves what does into a service — the value needs no
//! rights to write, and it is what Task Manager's startup list shows and lets
//! the user switch off. That switch lives in `StartupApproved\Run`, and an entry
//! switched off there does not start, so it reads as off here too; turning it
//! on in bliss clears the switch.

use std::path::Path;

/// The user's Run key.
const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// Where Task Manager keeps its switch for each of the Run key's entries.
const APPROVED: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
/// bliss's value in both.
const NAME: &str = "bliss";
/// The argument for a window that opens minimized; see `main.rs`.
pub const MINIMIZED: &str = "--minimized";

/// The two keys the entry lives in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keys {
    run: String,
    approved: String,
}

impl Keys {
    /// The real keys, or a scratch pair under the key `BLISS_STARTUP_KEY` names
    /// in HKCU: a probe clicks the checkbox without putting a test build among
    /// the programs Windows starts, or taking over the entry of the bliss the
    /// user runs.
    pub fn from_env() -> Self {
        match std::env::var("BLISS_STARTUP_KEY") {
            Ok(base) if !base.trim().is_empty() => Self::under(base.trim()),
            _ => Self {
                run: RUN.to_owned(),
                approved: APPROVED.to_owned(),
            },
        }
    }

    fn under(base: &str) -> Self {
        Self {
            run: format!(r"{base}\Run"),
            approved: format!(r"{base}\StartupApproved\Run"),
        }
    }
}

/// The entry, as the settings screen shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Startup {
    /// Windows starts bliss when the user signs in.
    pub on: bool,
    /// And its window opens minimized.
    pub minimized: bool,
    /// The entry starts a program other than this one, by its path: another
    /// copy of bliss, typically, a build moved or copied since.
    pub elsewhere: Option<String>,
    /// There is an entry, and Task Manager has switched it off.
    pub switched_off: bool,
    /// Why the entry could not be read.
    pub problem: Option<String>,
}

/// The value written for `exe`.
pub fn command_line(exe: &Path, minimized: bool) -> String {
    let mut line = format!("\"{}\"", exe.display());
    if minimized {
        line.push(' ');
        line.push_str(MINIMIZED);
    }
    line
}

/// The program an entry starts, and whether it asks for a minimized window. A
/// path with spaces comes quoted; one written by hand may not be.
fn parse(line: &str) -> Option<(String, bool)> {
    let line = line.trim();
    let (program, rest) = match line.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?,
        None => line.split_once(' ').unwrap_or((line, "")),
    };
    if program.trim().is_empty() {
        return None;
    }
    let minimized = rest.split_whitespace().any(|arg| arg == MINIMIZED);
    Some((program.to_owned(), minimized))
}

/// Whether `program` names `exe`. Windows paths ignore case.
fn same_program(program: &str, exe: &Path) -> bool {
    program.trim().to_lowercase() == exe.display().to_string().to_lowercase()
}

/// Task Manager's switch: the first byte is 2 (or 6) while the entry is on,
/// and 3 (or 7), followed by when, once the user switched it off.
fn switched_off(approved: &[u8]) -> bool {
    approved.first().is_some_and(|byte| byte & 1 == 1)
}

/// The entry as the screen shows it, from the two values as Windows keeps them.
fn describe(entry: Option<&str>, approved: Option<&[u8]>, exe: &Path) -> Startup {
    let Some(entry) = entry else {
        return Startup::default();
    };
    let off = approved.is_some_and(switched_off);
    let (program, minimized) = parse(entry).unwrap_or_else(|| (entry.to_owned(), false));
    Startup {
        on: !off,
        minimized,
        elsewhere: (!same_program(&program, exe)).then_some(program),
        switched_off: off,
        problem: None,
    }
}

/// Read the entry as it stands.
pub fn read(keys: &Keys, exe: &Path) -> Startup {
    match (
        imp::read_string(&keys.run, NAME),
        imp::read_binary(&keys.approved, NAME),
    ) {
        (Ok(entry), Ok(approved)) => describe(entry.as_deref(), approved.as_deref(), exe),
        (Err(e), _) | (_, Err(e)) => Startup {
            problem: Some(format!("не удалось прочитать автозапуск: {e}")),
            ..Startup::default()
        },
    }
}

/// Put bliss among the programs Windows starts, minimized or not, or take it
/// out. Turning it on also clears a switch Task Manager left off, or the entry
/// would be written and still never start.
pub fn set(keys: &Keys, exe: &Path, on: bool, minimized: bool) -> Result<(), String> {
    if on {
        imp::write_string(&keys.run, NAME, &command_line(exe, minimized))
    } else {
        imp::delete_value(&keys.run, NAME)
    }
    .map_err(|e| e.to_string())?;
    imp::delete_value(&keys.approved, NAME).map_err(|e| e.to_string())
}

#[cfg(windows)]
mod imp {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS,
        REG_SZ, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        RegSetValueExW,
    };
    use windows::core::{HSTRING, PCWSTR};

    /// A key handle that closes itself, so an early return cannot leak it.
    struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful open or create, and is
            // closed exactly once, here.
            unsafe {
                let _ = RegCloseKey(self.0);
            }
        }
    }

    fn error(status: WIN32_ERROR) -> std::io::Error {
        std::io::Error::from_raw_os_error(status.0 as i32)
    }

    /// `None` when there is no such key, which holds nothing.
    fn open(path: &str, access: REG_SAM_FLAGS) -> std::io::Result<Option<Key>> {
        let mut key = HKEY::default();
        let path = HSTRING::from(path);
        // SAFETY: the path outlives the call and `key` is a valid out-pointer.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                access,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status.is_err() {
            return Err(error(status));
        }
        Ok(Some(Key(key)))
    }

    fn create(path: &str) -> std::io::Result<Key> {
        let mut key = HKEY::default();
        let path = HSTRING::from(path);
        // SAFETY: as in `open`; no class, the default security, and no
        // disposition asked for.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        if status.is_err() {
            return Err(error(status));
        }
        Ok(Key(key))
    }

    /// A value's bytes, or `None` when there is no such value.
    pub fn read_binary(path: &str, name: &str) -> std::io::Result<Option<Vec<u8>>> {
        let Some(key) = open(path, KEY_READ)? else {
            return Ok(None);
        };
        let name = HSTRING::from(name);
        let mut len = 0u32;
        // SAFETY: a null data pointer with a length out-pointer is the
        // documented way to ask for the size.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut len),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status.is_err() {
            return Err(error(status));
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` is the `len` bytes the call just asked for.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
        };
        if status.is_err() {
            return Err(error(status));
        }
        buf.truncate(len as usize);
        Ok(Some(buf))
    }

    pub fn read_string(path: &str, name: &str) -> std::io::Result<Option<String>> {
        Ok(read_binary(path, name)?.map(|bytes| {
            let wide: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .take_while(|&unit| unit != 0)
                .collect();
            String::from_utf16_lossy(&wide)
        }))
    }

    pub fn write_string(path: &str, name: &str, value: &str) -> std::io::Result<()> {
        let key = create(path)?;
        let name = HSTRING::from(name);
        // The trailing NUL is part of a REG_SZ value.
        let bytes: Vec<u8> = value
            .encode_utf16()
            .chain(std::iter::once(0))
            .flat_map(u16::to_le_bytes)
            .collect();
        // SAFETY: the name and the bytes outlive the call; the length is theirs.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_SZ, Some(&bytes)) };
        if status.is_err() {
            return Err(error(status));
        }
        Ok(())
    }

    /// Taking out what is not there is not a failure.
    pub fn delete_value(path: &str, name: &str) -> std::io::Result<()> {
        let Some(key) = open(path, KEY_READ | KEY_WRITE)? else {
            return Ok(());
        };
        let name = HSTRING::from(name);
        // SAFETY: the name outlives the call.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if status.is_err() && status != ERROR_FILE_NOT_FOUND {
            return Err(error(status));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn write_binary(path: &str, name: &str, value: &[u8]) -> std::io::Result<()> {
        use windows::Win32::System::Registry::REG_BINARY;
        let key = create(path)?;
        let name = HSTRING::from(name);
        // SAFETY: as in `write_string`.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(value)) };
        if status.is_err() {
            return Err(error(status));
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn delete_tree(path: &str) {
        use windows::Win32::System::Registry::RegDeleteTreeW;
        let path = HSTRING::from(path);
        // SAFETY: the path outlives the call; a key that is not there is fine.
        unsafe {
            let _ = RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()));
        }
    }
}

#[cfg(not(windows))]
mod imp {
    fn unsupported() -> std::io::Error {
        std::io::Error::new(std::io::ErrorKind::Unsupported, "only on Windows")
    }

    pub fn read_binary(_path: &str, _name: &str) -> std::io::Result<Option<Vec<u8>>> {
        Err(unsupported())
    }

    pub fn read_string(_path: &str, _name: &str) -> std::io::Result<Option<String>> {
        Err(unsupported())
    }

    pub fn write_string(_path: &str, _name: &str, _value: &str) -> std::io::Result<()> {
        Err(unsupported())
    }

    pub fn delete_value(_path: &str, _name: &str) -> std::io::Result<()> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\Test User\Desktop\bliss.exe";

    #[test]
    fn the_command_line_quotes_the_path_and_reads_back() {
        let exe = Path::new(EXE);
        assert_eq!(command_line(exe, false), format!("\"{EXE}\""));
        assert_eq!(command_line(exe, true), format!("\"{EXE}\" --minimized"));
        assert_eq!(
            parse(&command_line(exe, true)),
            Some((EXE.to_owned(), true))
        );
        assert_eq!(
            parse(&command_line(exe, false)),
            Some((EXE.to_owned(), false))
        );
        // Written by hand without quotes: the first word is the program.
        assert_eq!(
            parse(r"C:\bliss\bliss.exe --minimized"),
            Some((r"C:\bliss\bliss.exe".to_owned(), true))
        );
        assert_eq!(parse("\"\""), None);
    }

    /// Task Manager's switch is a value bliss never writes, and an entry it
    /// switched off does not start: shown as on, it would be a promise Windows
    /// does not keep.
    #[test]
    fn an_entry_task_manager_switched_off_reads_as_off() {
        let exe = Path::new(EXE);
        let line = command_line(exe, true);
        let on = describe(
            Some(&line),
            Some(&[2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            exe,
        );
        assert!(
            on.on && on.minimized && !on.switched_off && on.elsewhere.is_none(),
            "{on:?}"
        );
        let off = describe(
            Some(&line),
            Some(&[3, 0, 0, 0, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x01]),
            exe,
        );
        assert!(!off.on && off.switched_off, "{off:?}");
        assert_eq!(describe(None, None, exe), Startup::default());
    }

    #[test]
    fn an_entry_for_another_copy_is_told_apart() {
        let exe = Path::new(EXE);
        let here = describe(
            Some(&command_line(
                Path::new(r"c:\users\test user\desktop\BLISS.EXE"),
                false,
            )),
            None,
            exe,
        );
        assert_eq!(here.elsewhere, None, "Windows paths ignore case");
        let other = describe(Some(r#""D:\builds\bliss.exe""#), None, exe);
        assert!(other.on);
        assert_eq!(other.elsewhere.as_deref(), Some(r"D:\builds\bliss.exe"));
    }

    /// The whole round on a scratch key under HKCU, never the real Run key: on,
    /// minimized, switched off by Task Manager, back on, and off.
    #[cfg(windows)]
    #[test]
    fn the_entry_round_trips_through_the_registry() {
        // One level under Software, so deleting it leaves no empty parent behind.
        let base = r"Software\bliss-test-startup-round-trip";
        imp::delete_tree(base);
        let keys = Keys::under(base);
        let exe = Path::new(EXE);

        assert_eq!(read(&keys, exe), Startup::default(), "nothing there yet");
        set(&keys, exe, true, false).expect("on");
        assert_eq!(
            imp::read_string(&keys.run, NAME).expect("reads"),
            Some(command_line(exe, false))
        );
        set(&keys, exe, true, true).expect("minimized");
        let now = read(&keys, exe);
        assert!(
            now.on && now.minimized && now.elsewhere.is_none(),
            "{now:?}"
        );

        imp::write_binary(&keys.approved, NAME, &[3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])
            .expect("switched off");
        assert!(read(&keys, exe).switched_off);
        set(&keys, exe, true, true).expect("on again");
        assert_eq!(
            imp::read_binary(&keys.approved, NAME).expect("reads"),
            None,
            "turning it on clears Task Manager's switch"
        );
        assert!(read(&keys, exe).on);

        set(&keys, exe, false, false).expect("off");
        assert_eq!(read(&keys, exe), Startup::default());
        set(&keys, exe, false, false).expect("off twice is still off, not an error");
        imp::delete_tree(base);
    }
}
