//! Publish a completed output without truncating an existing file on failure.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

struct PendingFile(Option<PathBuf>);

impl Drop for PendingFile {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn write_atomic(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
) -> io::Result<()> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    write_atomic_with_sequence(path, write, &NEXT)
}

fn write_atomic_with_sequence(
    path: &Path,
    write: impl FnOnce(&mut File) -> io::Result<()>,
    sequence: &AtomicU64,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (mut pending, mut file) = loop {
        let name = format!(
            ".fox-output-{}-{}.tmp",
            std::process::id(),
            sequence.fetch_add(1, Ordering::Relaxed)
        );
        // Keep the final path absent until publication, even when the caller
        // chooses a name from our temporary-file namespace.
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|destination| destination.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (PendingFile(Some(candidate)), file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    write(&mut file)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(
        pending.0.as_ref().expect("pending file owns its path"),
        path,
    )?;
    pending.0 = None;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn output_named_like_a_temporary_file_is_published_and_retained() {
        let directory =
            std::env::temp_dir().join(format!("fox-output-name-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let destination = directory.join(format!(".fox-output-{}-0.tmp", std::process::id()));
        write_atomic_with_sequence(
            &destination,
            |file| {
                assert!(
                    !destination.exists(),
                    "output was published before completion"
                );
                file.write_all(b"completed output")
            },
            &AtomicU64::new(0),
        )
        .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), b"completed output");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(destination).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn failed_writer_preserves_the_destination_and_removes_partial_bytes() {
        let directory =
            std::env::temp_dir().join(format!("fox-output-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let destination = directory.join("result.dat");
        std::fs::write(&destination, b"original").unwrap();
        let result = write_atomic(&destination, |file| {
            file.write_all(b"partial replacement")?;
            Err(io::Error::other("simulated write failure"))
        });
        assert!(result.is_err());
        assert_eq!(std::fs::read(&destination).unwrap(), b"original");
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        std::fs::remove_file(destination).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
