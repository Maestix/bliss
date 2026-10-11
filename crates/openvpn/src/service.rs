//! OpenVPN's interactive service: how a program without administrator rights
//! starts OpenVPN, and how OpenVPN GUI does.
//!
//! The service listens on `\\.\pipe\openvpn\service`. A client writes one
//! message — the working directory, OpenVPN's options and whatever OpenVPN
//! should read on standard input, each ending in a NUL, in UTF-16 — and the
//! service starts `openvpn.exe` as that client's user, adding one option of
//! its own, `--msg-channel`, over which OpenVPN asks it to do what needs the
//! rights: create the adapter, give it its address, add and remove routes.
//! The service answers with three lines: `0x00000000`, OpenVPN's process id
//! and `Process ID`, or an error code and what went wrong. If OpenVPN later
//! exits with an error it writes one more such message, then hangs up. When
//! OpenVPN exits it undoes everything it did on its behalf.
//!
//! The service takes any options and any profile from an account in the
//! Administrators or "OpenVPN Administrators" group; from anyone else only a
//! short list, and only profiles in OpenVPN's own folder. bliss writes its
//! profile in its own folder, so it needs the first. (`openvpnserv`'s
//! `interactive.c` and `validate.c`, read for this.)

use std::path::Path;

/// The service's name with Windows, and the pipe it answers on.
pub const SERVICE: &str = "OpenVPNServiceInteractive";

/// Whether OpenVPN's service is there to be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Availability {
    Ready,
    /// Installed, but stopped or disabled.
    Stopped,
    /// Not installed, or installed without its service.
    #[default]
    Missing,
}

/// What went wrong asking the service.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("служба OpenVPN не установлена — установите OpenVPN Community с openvpn.net")]
    Missing,
    #[error(
        "служба OpenVPN остановлена — запустите «OpenVPN Interactive Service» в службах Windows"
    )]
    Stopped,
    #[error("канал службы OpenVPN принадлежит не ей")]
    Impostor,
    #[error("служба OpenVPN отказала: {0}")]
    Refused(String),
    #[error("служба OpenVPN ответила непонятно")]
    Garbled,
    #[error("стартовые данные длиннее, чем служба OpenVPN принимает")]
    TooLong,
    #[error("не удалось поговорить со службой OpenVPN: {0}")]
    Io(#[from] std::io::Error),
}

/// OpenVPN, started.
pub struct Started {
    pub pid: u32,
    /// The service's pipe, left open: it says there if OpenVPN fails.
    pub pipe: imp::Pipe,
}

/// The message the service reads: each part ends in a NUL, in UTF-16.
///
/// The service refuses more than 4096 UTF-16 units, and one where anything
/// but the input is the last part.
pub fn startup_data(directory: &str, options: &str, input: &str) -> Result<Vec<u8>, ServiceError> {
    let units: Vec<u16> = [directory, options, input]
        .iter()
        .flat_map(|part| part.encode_utf16().chain(std::iter::once(0)))
        .collect();
    if units.len() > 4096 {
        return Err(ServiceError::TooLong);
    }
    Ok(units.iter().flat_map(|unit| unit.to_le_bytes()).collect())
}

/// The service's answer: OpenVPN's process id, or why there is none.
///
/// `0x%08x\n` and two more lines — for success the id, also in hex, then
/// `Process ID`; for a failure what failed and the system's words for it.
pub fn reply(text: &str) -> Result<u32, ServiceError> {
    let mut lines = text.split('\n');
    let code = hex(lines.next()).ok_or(ServiceError::Garbled)?;
    let second = lines.next().unwrap_or_default().trim();
    let third = lines.next().unwrap_or_default().trim();
    if code != 0 {
        let said = [second, third]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" — ");
        return Err(ServiceError::Refused(match code {
            // ValidateOptions: the account may not start this profile.
            0x2000_0001 if said.contains("admin approval") => {
                format!("учётной записи нужно быть в группе «OpenVPN Administrators» ({said})")
            }
            _ => said,
        }));
    }
    match (hex(Some(second)), third) {
        (Some(pid), "Process ID") if pid != 0 => Ok(pid),
        _ => Err(ServiceError::Garbled),
    }
}

fn hex(field: Option<&str>) -> Option<u32> {
    let field = field?.trim();
    u32::from_str_radix(field.strip_prefix("0x")?, 16).ok()
}

/// A path for OpenVPN's command line: quoted, since profiles live under the
/// user's folder and a user's name may hold spaces.
pub fn quoted(path: &Path) -> String {
    format!("\"{}\"", path.display())
}

pub use imp::{availability, start, terminate};

#[cfg(windows)]
mod imp {
    use std::path::Path;

    use windows::Win32::Foundation::{
        CloseHandle, ERROR_BROKEN_PIPE, ERROR_MORE_DATA, ERROR_PIPE_BUSY,
        ERROR_SERVICE_DOES_NOT_EXIST, GENERIC_READ, GENERIC_WRITE, HANDLE,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
    };
    use windows::Win32::System::Pipes::{
        GetNamedPipeServerProcessId, PIPE_READMODE_MESSAGE, SetNamedPipeHandleState,
    };
    use windows::Win32::System::Services::{
        CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatusEx, SC_HANDLE,
        SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO, SERVICE_QUERY_STATUS, SERVICE_RUNNING,
        SERVICE_STATUS_PROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_INFORMATION,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE, QueryFullProcessImageNameW,
        TerminateProcess,
    };
    use windows::core::{HSTRING, PWSTR, w};

    use super::{Availability, ServiceError, Started};

    /// The pipe, closed when dropped.
    pub struct Pipe(HANDLE);

    // SAFETY: a pipe handle is a kernel object's handle, usable from any
    // thread; it is read from one at a time.
    unsafe impl Send for Pipe {}

    impl Drop for Pipe {
        fn drop(&mut self) {
            let _ = unsafe { CloseHandle(self.0) };
        }
    }

    impl Pipe {
        /// Block until the service says something more, or hangs up.
        ///
        /// It writes again only when OpenVPN exits with an error, so this
        /// waits for as long as OpenVPN runs; a thread of its own.
        pub fn last_word(self) -> Option<String> {
            read_message(self.0).ok()
        }
    }

    struct Service(SC_HANDLE);

    impl Drop for Service {
        fn drop(&mut self) {
            let _ = unsafe { CloseServiceHandle(self.0) };
        }
    }

    /// The service's process, if it is running.
    fn service_pid() -> Result<u32, ServiceError> {
        let manager = Service(
            unsafe { OpenSCManagerW(None, None, SC_MANAGER_CONNECT) }
                .map_err(|e| ServiceError::Io(e.into()))?,
        );
        let service = match unsafe {
            OpenServiceW(
                manager.0,
                &HSTRING::from(super::SERVICE),
                SERVICE_QUERY_STATUS,
            )
        } {
            Ok(handle) => Service(handle),
            Err(e) if e.code() == ERROR_SERVICE_DOES_NOT_EXIST.to_hresult() => {
                return Err(ServiceError::Missing);
            }
            Err(e) => return Err(ServiceError::Io(e.into())),
        };
        let mut status = SERVICE_STATUS_PROCESS::default();
        let mut needed = 0u32;
        // SAFETY: the buffer is the structure this information level fills.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(
                (&raw mut status).cast::<u8>(),
                size_of::<SERVICE_STATUS_PROCESS>(),
            )
        };
        unsafe {
            QueryServiceStatusEx(service.0, SC_STATUS_PROCESS_INFO, Some(bytes), &mut needed)
        }
        .map_err(|e| ServiceError::Io(e.into()))?;
        if status.dwCurrentState != SERVICE_RUNNING || status.dwProcessId == 0 {
            return Err(ServiceError::Stopped);
        }
        Ok(status.dwProcessId)
    }

    pub fn availability() -> Availability {
        match service_pid() {
            Ok(_) => Availability::Ready,
            Err(ServiceError::Missing) => Availability::Missing,
            Err(_) => Availability::Stopped,
        }
    }

    /// Ask the service to start OpenVPN in `directory` with `options`.
    pub fn start(directory: &Path, options: &str) -> Result<Started, ServiceError> {
        let service = service_pid()?;
        let data = super::startup_data(&directory.display().to_string(), options, "")?;

        let pipe = open()?;
        // A pipe of this name answered by anything but the service would be
        // handed the path to a profile with a key in it.
        let mut server = 0u32;
        unsafe { GetNamedPipeServerProcessId(pipe.0, &mut server) }
            .map_err(|e| ServiceError::Io(e.into()))?;
        if server != service {
            return Err(ServiceError::Impostor);
        }
        let mode = PIPE_READMODE_MESSAGE;
        unsafe { SetNamedPipeHandleState(pipe.0, Some(&mode), None, None) }
            .map_err(|e| ServiceError::Io(e.into()))?;

        let mut written = 0u32;
        unsafe { WriteFile(pipe.0, Some(&data), Some(&mut written), None) }
            .map_err(|e| ServiceError::Io(e.into()))?;
        let answer = read_message(pipe.0)?;
        let pid = super::reply(&answer)?;
        Ok(Started { pid, pipe })
    }

    /// The pipe, waited for a little if every instance is busy: the service
    /// makes a new one for each client, a moment after the last was taken.
    fn open() -> Result<Pipe, ServiceError> {
        let mut attempts = 0;
        loop {
            let opened = unsafe {
                CreateFileW(
                    w!(r"\\.\pipe\openvpn\service"),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            };
            match opened {
                Ok(handle) => return Ok(Pipe(handle)),
                Err(e) if e.code() == ERROR_PIPE_BUSY.to_hresult() && attempts < 20 => {
                    attempts += 1;
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => return Err(ServiceError::Io(e.into())),
            }
        }
    }

    /// One whole message from the pipe, as text.
    fn read_message(pipe: HANDLE) -> Result<String, ServiceError> {
        let mut bytes = Vec::new();
        let mut buf = [0u8; 1024];
        loop {
            let mut read = 0u32;
            let result = unsafe { ReadFile(pipe, Some(&mut buf), Some(&mut read), None) };
            bytes.extend_from_slice(&buf[..read as usize]);
            match result {
                Ok(()) => break,
                // The message is longer than the buffer: the rest follows.
                Err(e) if e.code() == ERROR_MORE_DATA.to_hresult() => continue,
                Err(e) if e.code() == ERROR_BROKEN_PIPE.to_hresult() && !bytes.is_empty() => break,
                Err(e) => return Err(ServiceError::Io(e.into())),
            }
        }
        let units: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        Ok(String::from_utf16_lossy(&units))
    }

    /// End OpenVPN at once, for when asking did not: the service undoes its
    /// routes as soon as the process is gone. Only a process that is still
    /// `openvpn.exe` — the id may have been handed to another by then.
    ///
    /// The service's access list on the process gives the user TERMINATE and
    /// QUERY_INFORMATION but not QUERY_LIMITED_INFORMATION, and a request is
    /// refused whole if one bit is not granted: asked with LIMITED, as this
    /// first was, the open failed and OpenVPN was never ended.
    pub fn terminate(pid: u32) {
        let Some(process) = [PROCESS_QUERY_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION]
            .into_iter()
            .find_map(|query| unsafe { OpenProcess(PROCESS_TERMINATE | query, false, pid) }.ok())
        else {
            return;
        };
        let mut name = [0u16; 1024];
        let mut length = name.len() as u32;
        let named = unsafe {
            QueryFullProcessImageNameW(
                process,
                PROCESS_NAME_WIN32,
                PWSTR(name.as_mut_ptr()),
                &mut length,
            )
        };
        let is_openvpn = named.is_ok()
            && String::from_utf16_lossy(&name[..length as usize])
                .to_ascii_lowercase()
                .ends_with("\\openvpn.exe");
        if is_openvpn {
            let _ = unsafe { TerminateProcess(process, 1) };
        }
        let _ = unsafe { CloseHandle(process) };
    }
}

/// Linux has no service to ask: OpenVPN is started with root's rights
/// through polkit — `pkexec`, which asks for an administrator's password in
/// the desktop's own dialog — or directly by a bliss that is root itself.
///
/// `pkexec` replaces itself with OpenVPN, so the process bliss started is
/// OpenVPN's, by the same pid, with the standard input bliss writes the
/// profile into and the output it reads OpenVPN's words from. It also arms
/// the signal a child gets when its parent *thread* ends (`PR_SET_PDEATHSIG`):
/// started from one of tokio's threads, which come and go, OpenVPN could be
/// ended halfway through the password, so it is started from a thread that
/// lasts (`launch`).
#[cfg(target_os = "linux")]
pub mod linux {
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;
    use std::sync::mpsc;

    use tokio::process::{Child, Command};

    use super::Availability;

    /// OpenVPN, where distributions put it, or on the PATH.
    pub fn openvpn() -> Option<PathBuf> {
        [
            "/usr/sbin/openvpn",
            "/usr/bin/openvpn",
            "/usr/local/sbin/openvpn",
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .or_else(|| on_path("openvpn"))
    }

    fn on_path(name: &str) -> Option<PathBuf> {
        let path = std::env::var_os("PATH")?;
        std::env::split_paths(&path)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    /// Whether this process is root, and so starts OpenVPN itself.
    pub fn root() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    /// OpenVPN is there, and something can start it with the rights it
    /// needs. `Stopped` here means the second is missing: no polkit.
    pub fn availability() -> Availability {
        match (openvpn(), root() || on_path("pkexec").is_some()) {
            (None, _) => Availability::Missing,
            (Some(_), false) => Availability::Stopped,
            (Some(_), true) => Availability::Ready,
        }
    }

    /// What starts OpenVPN with `options`: `pkexec` and OpenVPN's full path,
    /// or OpenVPN itself for a bliss that is root.
    pub fn command(openvpn: &Path, options: &[String]) -> Command {
        let mut command = if root() {
            Command::new(openvpn)
        } else {
            let mut command = Command::new("pkexec");
            command.arg(openvpn);
            command
        };
        command
            .args(options)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            // Gone with bliss's own end of things, while it is still bliss's
            // to end: a pkexec waiting on the password. An OpenVPN running as
            // root is past it, and leaves on its leash instead.
            .kill_on_drop(true);
        command
    }

    type Task = Box<dyn FnOnce() + Send>;

    /// The thread OpenVPN is started from. It never exits.
    static LASTING: OnceLock<mpsc::Sender<Task>> = OnceLock::new();

    /// Start `command` from a thread that lasts as long as bliss does.
    pub async fn launch(mut command: Command) -> std::io::Result<Child> {
        let sender = LASTING.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Task>();
            std::thread::Builder::new()
                .name("bliss-openvpn".to_owned())
                .spawn(move || {
                    for task in rx {
                        task();
                    }
                })
                .expect("a thread to start OpenVPN from");
            tx
        });
        let runtime = tokio::runtime::Handle::current();
        let (tx, rx) = tokio::sync::oneshot::channel();
        sender
            .send(Box::new(move || {
                let _entered = runtime.enter();
                let _ = tx.send(command.spawn());
            }))
            .map_err(|_| std::io::Error::other("the thread OpenVPN is started from is gone"))?;
        rx.await
            .map_err(|_| std::io::Error::other("the thread OpenVPN is started from is gone"))?
    }

    /// What an exit before OpenVPN ever called says. `pkexec` answers 126
    /// when its dialog was dismissed and 127 when it may not, or cannot, ask
    /// — not authorized, or no polkit agent to show the dialog.
    pub fn exit_words(code: Option<i32>, through_pkexec: bool) -> String {
        match (code, through_pkexec) {
            (Some(126), true) => "запуск OpenVPN отменён в окне пароля".to_owned(),
            (Some(127), true) => "pkexec не дал запустить OpenVPN: нет прав администратора \
                                  или нет агента polkit, который спросил бы пароль"
                .to_owned(),
            (Some(code), _) => format!("OpenVPN завершился с ошибкой (exit code = {code})"),
            (None, _) => "OpenVPN завершился, остановленный сигналом".to_owned(),
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_refusal_by_polkit_says_which() {
            assert!(exit_words(Some(126), true).contains("отменён"));
            assert!(exit_words(Some(127), true).contains("polkit"));
            assert!(
                exit_words(Some(127), false).contains("exit code = 127"),
                "OpenVPN's own code, when there was no pkexec"
            );
            assert!(exit_words(Some(1), true).contains("exit code = 1"));
        }

        /// pkexec is told OpenVPN's full path: it looks up a bare name in a
        /// PATH of its own, not bliss's.
        #[test]
        fn pkexec_is_given_the_full_path() {
            let command = command(
                Path::new("/usr/sbin/openvpn"),
                &["--verb".into(), "3".into()],
            );
            let std = command.as_std();
            let args: Vec<_> = std
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect();
            if root() {
                assert_eq!(std.get_program(), "/usr/sbin/openvpn");
            } else {
                assert_eq!(std.get_program(), "pkexec");
                assert_eq!(args.first().map(String::as_str), Some("/usr/sbin/openvpn"));
            }
            assert!(args.ends_with(&["--verb".to_owned(), "3".to_owned()]));
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use std::path::Path;

    use super::{Availability, ServiceError, Started};

    pub struct Pipe;

    impl Pipe {
        pub fn last_word(self) -> Option<String> {
            None
        }
    }

    pub fn availability() -> Availability {
        super::linux::availability()
    }

    /// Linux starts OpenVPN through `linux::launch`, not here.
    pub fn start(_directory: &Path, _options: &str) -> Result<Started, ServiceError> {
        Err(ServiceError::Missing)
    }

    /// An OpenVPN running as root cannot be ended from here; it leaves on its
    /// leash (`session.rs`).
    pub fn terminate(_pid: u32) {}
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use std::path::Path;

    use super::{Availability, ServiceError, Started};

    pub struct Pipe;

    impl Pipe {
        pub fn last_word(self) -> Option<String> {
            None
        }
    }

    pub fn availability() -> Availability {
        Availability::Missing
    }

    pub fn start(_directory: &Path, _options: &str) -> Result<Started, ServiceError> {
        Err(ServiceError::Missing)
    }

    pub fn terminate(_pid: u32) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three parts, each ending in a NUL, in UTF-16 little-endian: the one
    /// shape the service reads, and its last unit must be the NUL.
    #[test]
    fn the_startup_data_is_three_terminated_parts() {
        let data = startup_data("C:\\d", "--verb 3", "").expect("short");
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .collect();
        let expected: Vec<u16> = "C:\\d\0--verb 3\0\0".encode_utf16().collect();
        assert_eq!(units, expected);
        assert_eq!(units.last(), Some(&0));
        assert!(matches!(
            startup_data("C:\\d", &"x".repeat(5000), ""),
            Err(ServiceError::TooLong)
        ));
    }

    #[test]
    fn the_reply_gives_the_process_id() {
        assert_eq!(
            reply("0x00000000\n0x00001f40\nProcess ID").ok(),
            Some(0x1f40)
        );
        assert!(matches!(reply("hello"), Err(ServiceError::Garbled)));
        assert!(matches!(
            reply("0x00000000\n0x00000000\nProcess ID"),
            Err(ServiceError::Garbled)
        ));
    }

    /// A refusal says why, and the one a user can fix says how.
    #[test]
    fn a_refusal_says_why() {
        let refused = reply(
            "0x20000001\nYou have specified a config file location (x relative to y) that \
             requires admin approval. This error may be avoided by adding your account to the \
             \"OpenVPN Administrators\" group\n",
        );
        let Err(ServiceError::Refused(why)) = refused else {
            panic!("{refused:?}");
        };
        assert!(why.contains("OpenVPN Administrators"), "{why}");
        let Err(ServiceError::Refused(why)) =
            reply("0x20000000\nOpenVPN exited with error: exit code = 1\n")
        else {
            panic!("not a refusal");
        };
        assert_eq!(why, "OpenVPN exited with error: exit code = 1");
    }

    #[test]
    fn paths_are_quoted() {
        assert_eq!(
            quoted(Path::new(r"C:\Users\A B\bliss\profile.ovpn")),
            r#""C:\Users\A B\bliss\profile.ovpn""#
        );
    }
}
