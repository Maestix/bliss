//! Starting with the session: bliss's entry among the programs started when
//! the user signs in — on Windows a value under the Run key, on Linux an XDG
//! autostart entry. Each platform's own switch for switching an entry off
//! without removing it is read as off, and turning bliss on clears it.

/// The argument for a window that opens minimized; see `main.rs`.
pub const MINIMIZED: &str = "--minimized";

/// The program an entry should start: this one. An AppImage runs from a
/// mount that is gone the moment it ends, under a name that changes every
/// start, so there the entry names the AppImage file itself, which its
/// runtime gives in `APPIMAGE`.
pub fn this_program() -> std::io::Result<std::path::PathBuf> {
    if cfg!(target_os = "linux")
        && let Some(image) = std::env::var_os("APPIMAGE").filter(|path| !path.is_empty())
    {
        return Ok(std::path::PathBuf::from(image));
    }
    std::env::current_exe()
}

/// The entry, as the settings screen shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Startup {
    /// The session starts bliss when the user signs in.
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
    /// Linux: whether bliss has an entry of its own in the application menu
    /// that starts this program. `None` where there is no such thing to
    /// offer: Windows' Start menu is the installer's, and bliss has none.
    pub menu: Option<bool>,
}

/// Windows: the value `bliss` under the user's Run key: this executable's
/// path in quotes, and `--minimized` for a window that opens minimized. A Run
/// value rather than the scheduled task the plan first named: the window needs
/// no elevation — stage 2 moves what does into a service — the value needs no
/// rights to write, and it is what Task Manager's startup list shows and lets
/// the user switch off. That switch lives in `StartupApproved\Run`, and an entry
/// switched off there does not start, so it reads as off here too; turning it
/// on in bliss clears the switch.
#[cfg(windows)]
mod run_key {
    use std::path::Path;

    use super::{MINIMIZED, Startup};

    /// The user's Run key.
    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    /// Where Task Manager keeps its switch for each of the Run key's entries.
    const APPROVED: &str =
        r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";
    /// bliss's value in both.
    const NAME: &str = "bliss";
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
            menu: None,
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
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| u16::from_le_bytes(*pair))
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
            let status = unsafe {
                RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(value))
            };
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
}

#[cfg(windows)]
pub use run_key::{Keys, read, set};

/// Windows has no menu entry of bliss's to offer (`Startup::menu`).
#[cfg(not(target_os = "linux"))]
pub fn set_menu(_keys: &Keys, _exe: &std::path::Path, _on: bool) -> Result<(), String> {
    Err("значок в меню приложений bliss ставит только на Linux".to_owned())
}

/// Linux: an XDG autostart entry, `bliss.desktop` in `~/.config/autostart`
/// (`$XDG_CONFIG_HOME/autostart`), which every desktop that follows the
/// freedesktop.org spec starts at sign-in and lists among its startup
/// applications. Its switch there is in the file itself: `Hidden=true` is
/// the spec's word for an entry taken away, `X-GNOME-Autostart-enabled=false`
/// GNOME's. Either reads as off, as Task Manager's switch does on Windows, and
/// turning bliss on writes the entry afresh without them.
#[cfg(target_os = "linux")]
mod xdg {
    use std::path::{Path, PathBuf};

    use super::{MINIMIZED, Startup};

    const FILE: &str = "bliss.desktop";

    /// Where the entries live: the one the session starts at sign-in, and the
    /// one in the application menu with its icon.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Keys {
        file: PathBuf,
        menu: PathBuf,
        icon: PathBuf,
    }

    impl Keys {
        /// The real folders, or a scratch one `BLISS_STARTUP_KEY` names: a
        /// probe clicks the checkboxes without putting a test build among the
        /// programs the session starts or the menu lists.
        pub fn from_env() -> Self {
            if let Some(dir) = std::env::var_os("BLISS_STARTUP_KEY").filter(|d| !d.is_empty()) {
                return Self::under(Path::new(&dir));
            }
            let base = directories::BaseDirs::new();
            let config = base
                .as_ref()
                .map(|base| base.config_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".config"));
            let data = base
                .as_ref()
                .map(|base| base.data_dir().to_path_buf())
                .unwrap_or_else(|| PathBuf::from(".local/share"));
            let id = app_id();
            Self {
                file: config.join("autostart").join(FILE),
                menu: data.join("applications").join(format!("{id}.desktop")),
                icon: data
                    .join("icons/hicolor/96x96/apps")
                    .join(format!("{id}.png")),
            }
        }

        fn under(dir: &Path) -> Self {
            Self {
                file: dir.join(FILE),
                menu: dir.join("applications").join(FILE),
                icon: dir.join("icons").join("bliss.png"),
            }
        }
    }

    /// The name a Wayland desktop knows the window by: GTK gives a window
    /// its program's name as its app id, and the desktop looks for a
    /// `<app id>.desktop` to take its icon and its name from.
    fn app_id() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.file_stem().map(|s| s.to_string_lossy().into_owned()))
            .filter(|stem| !stem.is_empty())
            .unwrap_or_else(|| "bliss".to_owned())
    }

    /// The characters an `Exec` line would need two layers of escaping for —
    /// the file's own and the command line's — and a path holds almost never.
    /// Refused rather than escaped wrong.
    const AWKWARD: [char; 7] = ['"', '`', '$', '\\', '%', '\n', '\r'];

    /// A path for an `Exec` line, or why it cannot be one.
    fn exec_path(exe: &Path) -> Result<String, String> {
        let path = exe.to_string_lossy();
        if let Some(c) = path.chars().find(|c| AWKWARD.contains(c)) {
            return Err(format!(
                "в пути к bliss есть «{c}», а строка запуска его не передаст: {path}"
            ));
        }
        Ok(format!("\"{path}\""))
    }

    /// The entry written for `exe`.
    pub fn entry(exe: &Path, minimized: bool) -> Result<String, String> {
        let mut exec = exec_path(exe)?;
        if minimized {
            exec.push(' ');
            exec.push_str(MINIMIZED);
        }
        Ok(format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=bliss\n\
             Comment=VLESS + REALITY client\n\
             Exec={exec}\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n"
        ))
    }

    /// What an entry says, in the `[Desktop Entry]` group: the program, its
    /// asking for the tray, and whether the desktop switched it off.
    fn parse(text: &str) -> Option<(String, bool, bool)> {
        let mut exec = None;
        let mut off = false;
        let mut in_entry = false;
        for line in text.lines().map(str::trim) {
            if line.starts_with('[') {
                in_entry = line == "[Desktop Entry]";
                continue;
            }
            if !in_entry {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match (key.trim(), value.trim()) {
                ("Exec", value) => exec = Some(value.to_owned()),
                ("Hidden", "true") | ("X-GNOME-Autostart-enabled", "false") => off = true,
                _ => {}
            }
        }
        let exec = exec?;
        let (program, rest) = match exec.strip_prefix('"') {
            Some(quoted) => quoted.split_once('"')?,
            None => exec.split_once(' ').unwrap_or((exec.as_str(), "")),
        };
        if program.trim().is_empty() {
            return None;
        }
        let minimized = rest.split_whitespace().any(|arg| arg == MINIMIZED);
        Some((program.to_owned(), minimized, off))
    }

    fn describe(text: &str, exe: &Path) -> Startup {
        let Some((program, minimized, off)) = parse(text) else {
            return Startup {
                problem: Some("запись автозапуска bliss не читается".to_owned()),
                ..Startup::default()
            };
        };
        Startup {
            on: !off,
            minimized,
            // Linux paths are compared as they are written: case matters.
            elsewhere: (Path::new(&program) != exe).then_some(program),
            switched_off: off,
            problem: None,
            // Read beside it, in `read`.
            menu: None,
        }
    }

    /// Read the entries as they stand.
    pub fn read(keys: &Keys, exe: &Path) -> Startup {
        let mut startup = match std::fs::read_to_string(&keys.file) {
            Ok(text) => describe(&text, exe),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Startup::default(),
            Err(e) => Startup {
                problem: Some(format!("не удалось прочитать автозапуск: {e}")),
                ..Startup::default()
            },
        };
        startup.menu = Some(menu_here(keys, exe));
        startup
    }

    /// The application menu's entry for `exe`, its icon by the file's path —
    /// no icon theme's cache to bring up to date — and the window's name for
    /// the desktop to match it by.
    fn menu_entry(exe: &Path, icon: &Path, id: &str) -> Result<String, String> {
        let exec = exec_path(exe)?;
        let icon = icon.to_string_lossy();
        if icon.contains(['\n', '\r']) {
            return Err("путь к значку не помещается в одну строку".to_owned());
        }
        Ok(format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=bliss\n\
             Comment=VLESS + REALITY client\n\
             Exec={exec}\n\
             Icon={icon}\n\
             Terminal=false\n\
             Categories=Network;\n\
             StartupNotify=true\n\
             StartupWMClass={id}\n"
        ))
    }

    /// Whether the menu has bliss's entry, and it starts this program.
    fn menu_here(keys: &Keys, exe: &Path) -> bool {
        std::fs::read_to_string(&keys.menu)
            .ok()
            .and_then(|text| parse(&text))
            .is_some_and(|(program, _, _)| Path::new(&program) == exe)
    }

    /// Put bliss in the application menu, with its icon, or take both out.
    /// On a click only, like the autostart entry: nothing of the kind is
    /// written on launch.
    pub fn set_menu(keys: &Keys, exe: &Path, on: bool) -> Result<(), String> {
        let gone = |path: &Path| match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
        if !on {
            return gone(&keys.menu)
                .and_then(|()| gone(&keys.icon))
                .map_err(|e| format!("не удалось убрать значок из меню: {e}"));
        }
        let text = menu_entry(exe, &keys.icon, &app_id())?;
        let written = || -> std::io::Result<()> {
            for path in [&keys.menu, &keys.icon] {
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
            }
            std::fs::write(&keys.icon, crate::ui::MARK)?;
            let part = keys.menu.with_extension("desktop.part");
            std::fs::write(&part, &text)?;
            std::fs::rename(&part, &keys.menu)
        };
        written().map_err(|e| format!("не удалось добавить значок в меню: {e}"))
    }

    /// Put bliss among the programs the session starts, minimized or not, or
    /// take it out. Turning it on writes the entry whole, so a switch the
    /// desktop left off goes with the old one.
    pub fn set(keys: &Keys, exe: &Path, on: bool, minimized: bool) -> Result<(), String> {
        if !on {
            return match std::fs::remove_file(&keys.file) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("не удалось убрать автозапуск: {e}"))
                }
                _ => Ok(()),
            };
        }
        let text = entry(exe, minimized)?;
        let written = || -> std::io::Result<()> {
            if let Some(dir) = keys.file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            // Beside it and renamed over it, so the session never finds half
            // an entry.
            let part = keys.file.with_extension("desktop.part");
            std::fs::write(&part, &text)?;
            std::fs::rename(&part, &keys.file)
        };
        written().map_err(|e| format!("не удалось записать автозапуск: {e}"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        const EXE: &str = "/home/test user/Apps/bliss";

        #[test]
        fn the_entry_quotes_the_path_and_reads_back() {
            let exe = Path::new(EXE);
            let text = entry(exe, true).expect("a plain path");
            assert!(text.contains("Exec=\"/home/test user/Apps/bliss\" --minimized\n"));
            assert_eq!(parse(&text), Some((EXE.to_owned(), true, false)));
            let text = entry(exe, false).expect("a plain path");
            assert_eq!(parse(&text), Some((EXE.to_owned(), false, false)));
            // Written by hand without quotes: the first word is the program.
            assert_eq!(
                parse("[Desktop Entry]\nExec=/opt/bliss/bliss --minimized\n"),
                Some(("/opt/bliss/bliss".to_owned(), true, false))
            );
            assert_eq!(parse("[Desktop Entry]\nName=bliss\n"), None);
        }

        /// A path the Exec line cannot carry is refused in words, not written
        /// so that the session starts something else.
        #[test]
        fn an_awkward_path_is_refused() {
            assert!(entry(Path::new("/home/a$b/bliss"), false).is_err());
            assert!(entry(Path::new("/home/100%/bliss"), false).is_err());
        }

        /// The desktop's own switch is in the entry, and an entry switched off
        /// there does not start: shown as on, it would be a promise the
        /// session does not keep. Keys of another group do not count.
        #[test]
        fn an_entry_the_desktop_switched_off_reads_as_off() {
            let exe = Path::new(EXE);
            let on = entry(exe, true).expect("an entry");
            assert!(describe(&on, exe).on);
            for switch in ["Hidden=true", "X-GNOME-Autostart-enabled=false"] {
                let off = format!("{on}{switch}\n");
                let read = describe(&off, exe);
                assert!(!read.on && read.switched_off, "{switch}: {read:?}");
            }
            let elsewhere = format!("{on}[Desktop Action other]\nHidden=true\n");
            assert!(describe(&elsewhere, exe).on);
        }

        #[test]
        fn an_entry_for_another_copy_is_told_apart() {
            let exe = Path::new(EXE);
            let other = entry(Path::new("/home/test user/builds/bliss"), false).expect("an entry");
            let read = describe(&other, exe);
            assert!(read.on);
            assert_eq!(
                read.elsewhere.as_deref(),
                Some("/home/test user/builds/bliss")
            );
            let upper = entry(Path::new("/home/test user/Apps/BLISS"), false).expect("an entry");
            assert!(
                describe(&upper, exe).elsewhere.is_some(),
                "case matters here"
            );
        }

        /// The menu's entry in a scratch folder, never the real menu: it
        /// starts this program, carries the mark beside it as its icon, reads
        /// as another copy's when it names another path, and goes with its
        /// icon when it is taken out.
        #[test]
        fn the_menu_entry_round_trips_through_the_folder() {
            let dir = std::env::temp_dir().join(format!("bliss-menu-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let keys = Keys::under(&dir);
            let exe = Path::new(EXE);

            assert_eq!(read(&keys, exe).menu, Some(false), "nothing there yet");
            set_menu(&keys, exe, true).expect("in the menu");
            assert_eq!(read(&keys, exe).menu, Some(true));
            let text = std::fs::read_to_string(&keys.menu).expect("written");
            assert!(
                text.contains("Exec=\"/home/test user/Apps/bliss\"\n"),
                "{text}"
            );
            assert!(
                text.contains(&format!("Icon={}\n", keys.icon.display())),
                "{text}"
            );
            assert_eq!(
                std::fs::read(&keys.icon).expect("the icon"),
                crate::ui::MARK
            );
            assert_eq!(
                read(&keys, Path::new("/opt/bliss/bliss")).menu,
                Some(false),
                "another copy's entry is not this one's"
            );

            set_menu(&keys, exe, false).expect("out of the menu");
            assert_eq!(read(&keys, exe).menu, Some(false));
            assert!(!keys.icon.exists(), "the icon goes with it");
            set_menu(&keys, exe, false).expect("out twice is still out, not an error");
            let _ = std::fs::remove_dir_all(&dir);
        }

        /// The whole round in a scratch folder, never the real autostart:
        /// on, minimized, switched off by the desktop, back on, and off.
        #[test]
        fn the_entry_round_trips_through_the_folder() {
            let dir = std::env::temp_dir().join(format!("bliss-autostart-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let keys = Keys::under(&dir);
            let exe = Path::new(EXE);

            let nothing = Startup {
                menu: Some(false),
                ..Startup::default()
            };
            assert_eq!(read(&keys, exe), nothing, "nothing there yet");
            set(&keys, exe, true, false).expect("on");
            set(&keys, exe, true, true).expect("minimized");
            let now = read(&keys, exe);
            assert!(
                now.on && now.minimized && now.elsewhere.is_none(),
                "{now:?}"
            );

            let text = std::fs::read_to_string(&keys.file).expect("written");
            std::fs::write(&keys.file, format!("{text}Hidden=true\n")).expect("switched off");
            assert!(read(&keys, exe).switched_off);
            set(&keys, exe, true, true).expect("on again");
            assert!(
                read(&keys, exe).on,
                "turning it on clears the desktop's switch"
            );

            set(&keys, exe, false, false).expect("off");
            assert_eq!(read(&keys, exe), nothing);
            set(&keys, exe, false, false).expect("off twice is still off, not an error");
            let left: Vec<_> = std::fs::read_dir(&dir).expect("the folder").collect();
            assert!(left.is_empty(), "nothing left behind: {left:?}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

#[cfg(target_os = "linux")]
pub use xdg::{Keys, read, set, set_menu};

#[cfg(not(any(windows, target_os = "linux")))]
mod elsewhere {
    use std::path::Path;

    use super::Startup;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Keys;

    impl Keys {
        pub fn from_env() -> Self {
            Self
        }
    }

    pub fn read(_keys: &Keys, _exe: &Path) -> Startup {
        Startup {
            problem: Some("автозапуск на этой системе не поддерживается".to_owned()),
            ..Startup::default()
        }
    }

    pub fn set(_keys: &Keys, _exe: &Path, _on: bool, _minimized: bool) -> Result<(), String> {
        Err("автозапуск на этой системе не поддерживается".to_owned())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
pub use elsewhere::{Keys, read, set};
