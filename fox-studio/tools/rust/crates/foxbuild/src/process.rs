//! Managed children for snapshots, interpreter probes and stages. Dropping a live
//! child terminates its managed tree and reaps the direct child.
use crate::cancel::Cancellation;
use anyhow::{Context, Result};
use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

#[derive(Clone, Copy)]
pub(crate) enum Priority {
    BelowNormal,
    Idle,
}

pub(crate) struct ManagedChild {
    child: Child,
    reaped: bool,
    #[cfg(windows)]
    job: crate::job::Job,
}

impl ManagedChild {
    pub(crate) fn spawn(command: &mut Command, priority: Priority) -> Result<Self> {
        #[cfg(windows)]
        let (child, job) = {
            use std::os::windows::process::CommandExt;
            use windows_sys::Win32::System::Threading::{
                BELOW_NORMAL_PRIORITY_CLASS, CREATE_NO_WINDOW, CREATE_SUSPENDED,
                IDLE_PRIORITY_CLASS,
            };
            let priority = match priority {
                Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
                Priority::Idle => IDLE_PRIORITY_CLASS,
            };
            let job = crate::job::Job::new().context("creating a managed process job")?;
            command.creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED | priority);
            let mut child = command.spawn()?;
            if let Err(error) = job.add(&child).and_then(|()| crate::job::resume(&child)) {
                let _ = job.terminate();
                let _ = child.kill();
                let _ = child.wait();
                return Err(error).context("containing and resuming the child process");
            }
            (child, job)
        };
        #[cfg(not(windows))]
        let child = {
            let _ = priority;
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            command.spawn()?
        };
        Ok(Self {
            child,
            reaped: false,
            #[cfg(windows)]
            job,
        })
    }

    pub(crate) fn id(&self) -> u32 {
        self.child.id()
    }

    fn terminate_tree(&mut self) -> io::Result<()> {
        #[cfg(windows)]
        {
            self.job.terminate()
        }
        #[cfg(unix)]
        {
            unsafe extern "C" {
                fn kill(pid: i32, signal: i32) -> i32;
            }
            // The child created its own group (PGID = PID). Descendants stay in
            // it unless they deliberately detach; detached services are outside
            // the stage contract.
            let result = unsafe { kill(-(self.child.id() as i32), 9) };
            if result == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                // ESRCH: the group already exited.
                Ok(())
            } else {
                Err(error)
            }
        }
        #[cfg(not(any(windows, unix)))]
        {
            self.child.kill()
        }
    }

    pub(crate) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.child.try_wait()?;
        if result.is_some() && !self.reaped {
            self.terminate_tree()?;
            self.reaped = true;
        }
        Ok(result)
    }

    pub(crate) fn cancel(&mut self) -> io::Result<()> {
        if !self.reaped {
            self.terminate_tree()?;
            self.child.wait()?;
            self.reaped = true;
        }
        Ok(())
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.cancel();
    }
}

pub(crate) struct Captured {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Drain pipes while polling the marker so a verbose probe cannot deadlock the
/// scheduler. None means cancellation, including cancellation before spawn.
pub(crate) fn capture(command: &mut Command, cancel: &Cancellation) -> Result<Option<Captured>> {
    if cancel.requested()? {
        return Ok(None);
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = ManagedChild::spawn(command, Priority::BelowNormal)?;
    let stdout = child
        .child
        .stdout
        .take()
        .context("child stdout pipe missing")?;
    let stderr = child
        .child
        .stderr
        .take()
        .context("child stderr pipe missing")?;
    let read = |mut stream: Box<dyn Read + Send>| {
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).map(|_| bytes)
    };
    let stdout = std::thread::spawn(move || read(Box::new(stdout)));
    let stderr = std::thread::spawn(move || read(Box::new(stderr)));
    let status = loop {
        if cancel.requested()? {
            child.cancel().context("stopping the cancelled probe")?;
            break None;
        }
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        cancel.sleep(Duration::from_millis(50));
    };
    let stdout = stdout
        .join()
        .map_err(|_| anyhow::anyhow!("stdout reader panicked"))??;
    let stderr = stderr
        .join()
        .map_err(|_| anyhow::anyhow!("stderr reader panicked"))??;
    Ok(status.map(|status| Captured {
        status,
        stdout,
        stderr,
    }))
}
