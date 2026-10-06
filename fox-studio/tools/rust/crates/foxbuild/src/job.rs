//! Windows process containment. Attach a suspended child before any of its code runs.
#[cfg(windows)]
use std::io;
#[cfg(windows)]
pub(crate) struct Job(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Job {
    pub(crate) fn new() -> io::Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        unsafe {
            let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if h.is_null() {
                return Err(io::Error::last_os_error());
            }
            let job = Self(h);
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(job)
        }
    }

    pub(crate) fn add(&self, child: &std::process::Child) -> io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        unsafe {
            if windows_sys::Win32::System::JobObjects::AssignProcessToJobObject(
                self.0,
                child.as_raw_handle() as _,
            ) == 0
            {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub(crate) fn terminate(&self) -> io::Result<()> {
        unsafe {
            if windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 130) == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(windows)]
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
pub(crate) fn resume(child: &std::process::Child) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtResumeProcess(handle: windows_sys::Win32::Foundation::HANDLE) -> i32;
    }
    // std retains the process handle, not the primary thread handle. The same
    // native resume operation is used by the existing PAUSE support.
    let status = unsafe { NtResumeProcess(child.as_raw_handle() as _) };
    if status < 0 {
        return Err(io::Error::other(format!(
            "NtResumeProcess failed with NTSTATUS {status:#x}"
        )));
    }
    Ok(())
}
