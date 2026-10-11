//! Tying the engine's lifetime to ours, so it cannot outlive us.
//!
//! `kill_on_drop` covers a normal exit and a panic, because `Drop` runs. It does
//! not cover `TerminateProcess` — a Stop-Process, a Task Manager "End task", a
//! debugger detaching badly — and that was not theoretical: a force-killed bliss
//! left an xray running and holding the bridge port, so the next start failed
//! with a port clash that pointed nowhere near the real cause. `kill -9` is the
//! same on Linux.
//!
//! So the kernel is asked to end the engine when bliss ends, however it ends:
//! on Windows by a Job Object with `KILL_ON_JOB_CLOSE`, on Linux by the signal
//! a child is sent when its parent dies (`PR_SET_PDEATHSIG`). [`spawn`] starts
//! a command on that leash and hands back what has to be kept for it to hold.

use tokio::process::{Child, Command};

/// Start `command` on the leash. Keep the [`Held`] for as long as the child
/// should live; dropping it may end the child.
pub async fn spawn(command: Command) -> std::io::Result<(Child, Held)> {
    imp::spawn(command).await
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use tokio::process::{Child, Command};
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// The job, when there is one. Dropping it closes the handle, which kills
    /// the members.
    pub struct Held(#[allow(dead_code)] Option<Job>);

    pub async fn spawn(mut command: Command) -> std::io::Result<(Child, Held)> {
        let job = match Job::kill_on_close() {
            Ok(job) => Some(job),
            Err(e) => {
                tracing::warn!("no job object ({e}) — a force-killed bliss may leak the engine");
                None
            }
        };

        let child = command.spawn()?;

        // Assigned immediately after spawn. There is a hair of a window before
        // this in which the child could spawn its own children outside the job;
        // closing it needs CREATE_SUSPENDED, which tokio does not expose, and
        // xray spawns nothing, so the window is not worth the machinery.
        if let (Some(job), Some(handle)) = (job.as_ref(), child.raw_handle()) {
            // SAFETY: the handle comes from the child we just created, which
            // carries the rights AssignProcessToJobObject needs.
            if let Err(e) = unsafe { job.adopt(handle as isize) } {
                tracing::warn!("could not put the engine in the job object: {e}");
            }
        }
        Ok((child, Held(job)))
    }

    /// Owns the job.
    struct Job(HANDLE);

    // The handle is only ever used through these calls, all of which are
    // thread-safe kernel operations.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        fn kill_on_close() -> windows::core::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(None, None)?;

                let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &raw const info as *const c_void,
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )?;

                Ok(Self(job))
            }
        }

        /// # Safety
        /// `process` must be a live process handle with `PROCESS_SET_QUOTA` and
        /// `PROCESS_TERMINATE` rights — which is what `CreateProcess` hands back
        /// for a child we spawned.
        unsafe fn adopt(&self, process: isize) -> windows::core::Result<()> {
            unsafe { AssignProcessToJobObject(self.0, HANDLE(process as *mut c_void)) }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
}

/// Linux has no job objects. It has `PR_SET_PDEATHSIG`: set in the child
/// between fork and exec, it has the kernel send the child a signal when its
/// parent goes. Two things about it shape this module.
///
/// - **"Parent" is the thread that forked, not the process** (prctl(2)). tokio
///   forks on whichever thread the spawning task runs on, and its threads come
///   and go: a worker that hands its core away in `block_in_place` — ipstack
///   does that — becomes a blocking thread, which exits after ten idle
///   seconds. An engine forked there would be killed for no reason anyone
///   could see. So the fork happens on one thread that lives as long as bliss
///   (`LEASH`), with the runtime entered for tokio's bookkeeping.
/// - **It is set after the fork**, so a bliss that died in between has already
///   gone and the signal will never come: the child checks its parent is
///   still the one that forked it, and refuses to start otherwise.
#[cfg(target_os = "linux")]
mod imp {
    use std::os::unix::process::CommandExt;
    use std::sync::OnceLock;
    use std::sync::mpsc;

    use tokio::process::{Child, Command};

    /// Nothing to keep: the leash is the kernel's.
    pub struct Held;

    type Task = Box<dyn FnOnce() + Send>;

    /// The thread every leashed child is forked from. It never exits, so its
    /// children are signalled only when the whole process goes.
    static LEASH: OnceLock<mpsc::Sender<Task>> = OnceLock::new();

    fn leash_thread() -> &'static mpsc::Sender<Task> {
        LEASH.get_or_init(|| {
            let (tx, rx) = mpsc::channel::<Task>();
            std::thread::Builder::new()
                .name("bliss-leash".to_owned())
                .spawn(move || {
                    for task in rx {
                        task();
                    }
                })
                .expect("a thread for the engine's leash");
            tx
        })
    }

    pub async fn spawn(mut command: Command) -> std::io::Result<(Child, Held)> {
        let parent = std::process::id() as libc::pid_t;
        // SAFETY: the closure calls only prctl and getppid, both
        // async-signal-safe, and allocates nothing — which is all a child may
        // do between fork and exec.
        unsafe {
            command.as_std_mut().pre_exec(move || pull_on_death(parent));
        }

        let runtime = tokio::runtime::Handle::current();
        let (tx, rx) = tokio::sync::oneshot::channel();
        leash_thread()
            .send(Box::new(move || {
                let _entered = runtime.enter();
                let _ = tx.send(command.spawn());
            }))
            .map_err(|_| std::io::Error::other("the engine's leash thread is gone"))?;
        let child = rx
            .await
            .map_err(|_| std::io::Error::other("the engine's leash thread is gone"))??;
        Ok((child, Held))
    }

    /// In the child, before exec: die with the parent.
    fn pull_on_death(parent: libc::pid_t) -> std::io::Result<()> {
        // SAFETY: plain syscalls with integer arguments.
        if unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) } == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // The parent went between fork and prctl: no signal will come for it.
        // A preallocated error, since nothing may allocate here.
        if unsafe { libc::getppid() } != parent {
            return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
        }
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
mod imp {
    use tokio::process::{Child, Command};

    /// Elsewhere there is no leash: `kill_on_drop` alone.
    pub struct Held;

    pub async fn spawn(mut command: Command) -> std::io::Result<(Child, Held)> {
        Ok((command.spawn()?, Held))
    }
}

pub use imp::Held;

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::time::Duration;

    /// The engine goes when the thread that started it goes — unless it was
    /// started on the leash thread. A child forked from a short-lived thread
    /// and leashed there would be killed with that thread; the test forks from
    /// one, lets it end, and expects the child alive.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_engine_outlives_the_thread_that_asked_for_it() {
        let runtime = tokio::runtime::Handle::current();
        let child = std::thread::spawn(move || {
            runtime.block_on(async {
                let mut command = tokio::process::Command::new("sleep");
                command.arg("30").kill_on_drop(true);
                super::spawn(command).await.expect("sleep starts").0
            })
        })
        .join()
        .expect("the asking thread ends");
        let mut child = child;
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            child.try_wait().expect("the child can be asked").is_none(),
            "the child died with the thread that asked for it"
        );
        child.kill().await.expect("the child is ended");
    }

    /// Not a test of its own: the bliss that `the_engine_dies_with_bliss`
    /// kills. Run as a separate process, and only when asked to be.
    #[tokio::test]
    async fn stand_in_for_bliss() {
        if std::env::var_os(STAND_IN).is_none() {
            return;
        }
        let mut command = tokio::process::Command::new("sleep");
        command.arg("30");
        let (child, _held) = super::spawn(command).await.expect("sleep starts");
        println!("child {}", child.id().expect("a pid"));
        tokio::time::sleep(Duration::from_secs(30)).await;
    }

    const STAND_IN: &str = "BLISS_LEASH_STAND_IN";

    /// And it goes with bliss: a stand-in killed with SIGKILL — which runs no
    /// code at all, as `kill -9` or the OOM killer would — takes the engine it
    /// started with it. Seen failing with the prctl taken out.
    #[test]
    fn the_engine_dies_with_bliss() {
        use std::io::BufRead;

        let mut bliss = std::process::Command::new(std::env::current_exe().expect("the test"))
            .args(["--exact", "job::tests::stand_in_for_bliss", "--nocapture"])
            .env(STAND_IN, "1")
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("the stand-in starts");
        let out = std::io::BufReader::new(bliss.stdout.take().expect("its output"));
        let pid: libc::pid_t = out
            .lines()
            .map_while(Result::ok)
            .find_map(|line| line.strip_prefix("child ").and_then(|p| p.parse().ok()))
            .expect("the stand-in names its engine");
        let alive = || {
            // A zombie still answers kill(0); one that was reaped is gone, and
            // one waiting to be reaped says so in its state.
            std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
                stat.rsplit(')')
                    .next()
                    .is_some_and(|s| !s.trim_start().starts_with('Z'))
            })
        };
        assert!(alive(), "the engine runs while bliss does");

        bliss.kill().expect("SIGKILL");
        bliss.wait().expect("reaped");
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while alive() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if alive() {
            // SAFETY: a plain kill(2) of the child this test left behind.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            panic!("the engine outlived a bliss killed with SIGKILL");
        }
    }
}
