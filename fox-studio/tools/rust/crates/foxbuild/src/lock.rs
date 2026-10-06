//! Cross-process ownership of build state and outputs.
//!
//! The persistent lease file must never be removed: unlinking a locked inode
//! would allow a second launcher to lock a replacement while the first runs.
//! The separate PID file stays compatible with status/pause tools and is
//! published and removed only while the OS lease is held.
use anyhow::{Context, Result, bail};
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use sysinfo::{Pid, ProcessesToUpdate, System};

pub(crate) struct Lease {
    _file: File,
}

impl Lease {
    pub(crate) fn try_acquire(log_dir: &Path) -> Result<Option<Self>> {
        let path = log_dir.join("foxbuild.lease");
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("opening build lease {}", path.display()))?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => {
                Err(error).with_context(|| format!("locking build lease {}", path.display()))
            }
        }
    }
}

/// Exclusive publication of a project's shared writable inputs.
///
/// Acquire using the selected graph's configured log directory before any
/// shared graph/facet write. A busy build/publication returns None without
/// claiming PID metadata. The persistent lease file must never be unlinked.
///
/// Keep inspection snapshots private and immutable. Drop this guard before
/// spawning foxbuild: the scheduler acquires its own lease. Graph and facet/
/// spec input snapshots must stay immutable for the entire child lifetime,
/// including the handoff. This guard is not a transferable child-process lease.
pub struct PublicationLease {
    _lease: Lease,
}

impl PublicationLease {
    pub fn try_acquire(log_dir: &Path) -> Result<Option<Self>> {
        fs::create_dir_all(log_dir)?;
        let Some(lease) = Lease::try_acquire(log_dir)? else {
            return Ok(None);
        };
        // Preserve a running legacy PID-only scheduler as well. Modern
        // schedulers cannot acquire while our OS lease is held.
        if holder(log_dir).is_some() {
            return Ok(None);
        }
        Ok(Some(Self { _lease: lease }))
    }
}

pub(crate) struct BuildLock {
    // Field drop closes the held handle only after our Drop releases the PID.
    _lease: Lease,
    path: PathBuf,
    owner: String,
}

impl BuildLock {
    pub(crate) fn acquire(log_dir: &Path) -> Result<Self> {
        let Some(lease) = Lease::try_acquire(log_dir)? else {
            bail!("another foxbuild is running on this repo (build lease is held)");
        };
        let path = log_dir.join("foxbuild.lock");
        match fs::read_to_string(&path) {
            Ok(text) => {
                // An older scheduler/audit may publish only PID metadata. Keep
                // refusing live legacy owners rather than taking their work.
                if let Ok(pid) = text.trim().parse::<u32>()
                    && process_alive(pid)
                {
                    bail!("another foxbuild (pid {pid}) is running on this repo");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("reading build owner {}", path.display()));
            }
        }

        let owner = std::process::id().to_string();
        // Atomic publication also keeps unlocked PID readers from observing
        // the empty-file window that the old create_new protocol exposed.
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let temporary = log_dir.join(format!("foxbuild.lock.{owner}.{nonce}.tmp"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| format!("creating build owner {}", temporary.display()))?;
        let published = (|| -> Result<()> {
            file.write_all(owner.as_bytes())?;
            file.flush()?;
            drop(file);
            fs::rename(&temporary, &path)?;
            Ok(())
        })();
        if let Err(error) = published {
            let _ = fs::remove_file(&temporary);
            return Err(error).context("publishing build owner");
        }
        Ok(Self {
            _lease: lease,
            path,
            owner,
        })
    }
}

impl Drop for BuildLock {
    fn drop(&mut self) {
        // A failed contender never gets this guard. Do not remove metadata
        // replaced by an external owner while we held our own lease.
        if fs::read_to_string(&self.path).is_ok_and(|text| text == self.owner) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn holder(log_dir: &Path) -> Option<u32> {
    let text = fs::read_to_string(log_dir.join("foxbuild.lock")).ok()?;
    let pid = text.trim().parse().ok()?;
    process_alive(pid).then_some(pid)
}

fn process_alive(pid: u32) -> bool {
    let mut system = System::new();
    system.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    system.process(Pid::from_u32(pid)).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let path =
                std::env::temp_dir().join(format!("foxbuild_lock_{}_{unique}", std::process::id()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn simultaneous_empty_and_stale_recovery_admits_exactly_one_owner() {
        let fixture = Fixture::new();
        for previous in ["", "4294967295"] {
            fs::write(fixture.0.join("foxbuild.lock"), previous).unwrap();
            let start = Arc::new(Barrier::new(2));
            let attempted = Arc::new(Barrier::new(2));
            let results = std::thread::scope(|scope| {
                let mut threads = Vec::new();
                for _ in 0..2 {
                    let start = start.clone();
                    let attempted = attempted.clone();
                    let path = &fixture.0;
                    threads.push(scope.spawn(move || {
                        start.wait();
                        let guard = BuildLock::acquire(path);
                        // Keep the winning lease held until both contenders
                        // have attempted ownership.
                        attempted.wait();
                        assert_eq!(
                            fs::read_to_string(path.join("foxbuild.lock")).unwrap(),
                            std::process::id().to_string()
                        );
                        let acquired = guard.is_ok();
                        // Keep the winner alive through the loser's assertion.
                        start.wait();
                        acquired
                    }));
                }
                threads
                    .into_iter()
                    .map(|thread| thread.join().unwrap())
                    .collect::<Vec<_>>()
            });
            assert_eq!(results.into_iter().filter(|&acquired| acquired).count(), 1);
            assert!(!fixture.0.join("foxbuild.lock").exists());
            assert!(fixture.0.join("foxbuild.lease").exists());
            drop(BuildLock::acquire(&fixture.0).unwrap());
        }
    }

    #[test]
    fn releasing_a_guard_does_not_remove_replaced_owner_metadata() {
        let fixture = Fixture::new();
        let guard = BuildLock::acquire(&fixture.0).unwrap();
        let replacement = "external replacement";
        fs::write(fixture.0.join("foxbuild.lock"), replacement).unwrap();
        drop(guard);
        assert_eq!(
            fs::read_to_string(fixture.0.join("foxbuild.lock")).unwrap(),
            replacement
        );
        drop(BuildLock::acquire(&fixture.0).unwrap());
    }

    #[test]
    fn publication_lease_excludes_build_and_legacy_owners_without_pid_publication() {
        let fixture = Fixture::new();
        let build = BuildLock::acquire(&fixture.0).unwrap();
        assert!(PublicationLease::try_acquire(&fixture.0).unwrap().is_none());
        drop(build);

        let publication = PublicationLease::try_acquire(&fixture.0).unwrap().unwrap();
        assert!(!fixture.0.join("foxbuild.lock").exists());
        assert!(BuildLock::acquire(&fixture.0).is_err());
        assert!(PublicationLease::try_acquire(&fixture.0).unwrap().is_none());
        drop(publication);
        drop(BuildLock::acquire(&fixture.0).unwrap());

        let legacy = std::process::id().to_string();
        fs::write(fixture.0.join("foxbuild.lock"), &legacy).unwrap();
        assert!(PublicationLease::try_acquire(&fixture.0).unwrap().is_none());
        assert_eq!(
            fs::read_to_string(fixture.0.join("foxbuild.lock")).unwrap(),
            legacy
        );
        fs::remove_file(fixture.0.join("foxbuild.lock")).unwrap();
        drop(PublicationLease::try_acquire(&fixture.0).unwrap().unwrap());
        assert!(fixture.0.join("foxbuild.lease").exists());
    }

    #[test]
    fn live_legacy_owner_is_preserved() {
        let fixture = Fixture::new();
        let owner = std::process::id().to_string();
        fs::write(fixture.0.join("foxbuild.lock"), &owner).unwrap();
        assert!(BuildLock::acquire(&fixture.0).is_err());
        assert_eq!(
            fs::read_to_string(fixture.0.join("foxbuild.lock")).unwrap(),
            owner
        );
        fs::remove_file(fixture.0.join("foxbuild.lock")).unwrap();
        drop(BuildLock::acquire(&fixture.0).unwrap());
    }
}
