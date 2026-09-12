//! Tying the engine's lifetime to ours, so it cannot outlive us.
//!
//! `kill_on_drop` covers a normal exit and a panic, because `Drop` runs. It does
//! not cover `TerminateProcess` — a Stop-Process, a Task Manager "End task", a
//! debugger detaching badly — and that was not theoretical: a force-killed bliss
//! left an xray running and holding the bridge port, so the next start failed
//! with a port clash that pointed nowhere near the real cause.
//!
//! A Job Object with `KILL_ON_JOB_CLOSE` moves the decision into the kernel. We
//! hold the only handle; however our process dies, Windows closes that handle,
//! and everything in the job goes with it.

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject,
    };

    /// Owns the job. Dropping it closes the handle, which kills the members.
    pub struct Job(HANDLE);

    // The handle is only ever used through these calls, all of which are
    // thread-safe kernel operations.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Job {
        pub fn kill_on_close() -> windows::core::Result<Self> {
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
        pub unsafe fn adopt(&self, process: isize) -> windows::core::Result<()> {
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

#[cfg(not(windows))]
mod imp {
    /// Elsewhere this is a no-op: bliss targets Windows, and a process group
    /// would be the equivalent if that ever changes.
    pub struct Job;

    impl Job {
        pub fn kill_on_close() -> Result<Self, std::convert::Infallible> {
            Ok(Self)
        }
        /// # Safety
        /// Trivially safe; the signature matches the Windows one.
        pub unsafe fn adopt(&self, _process: isize) -> Result<(), std::convert::Infallible> {
            Ok(())
        }
    }
}

pub use imp::Job;
