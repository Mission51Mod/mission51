//! Deterministic .mgsv packages from a MakeBite staging folder.
//!
//! Packages contain generated metadata.xml, archive assets and GameDir loose
//! files. Fixed DOS timestamps and sorted staging order preserve byte identity.
//! Inputs are protected before writing; a completed, synced temporary file is
//! renamed into place. Classic ZIP limits are checked instead of writing ZIP64.

use crate::mgsv::ModPackage;
use rayon::prelude::*;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DOS_TIME: u16 = 0;
const DOS_DATE: u16 = ((2015 - 1980) << 9) | (9 << 5) | 1;
const ZIP64_REQUIRED: &str = "package exceeds classic ZIP limits; ZIP64 is not supported";
static NEXT_OUTPUT: AtomicU64 = AtomicU64::new(0);

struct Entry {
    name: String,
    crc: u32,
    size: u64,
    method: u16,
    data: Vec<u8>,
}

struct DirectoryEntry {
    name: String,
    crc: u32,
    compressed_size: u32,
    size: u32,
    method: u16,
    offset: u32,
}

fn pack_entry(name: String, raw: Vec<u8>, level: u32) -> Result<Entry, String> {
    let crc = crc32fast::hash(&raw);
    let size = raw.len() as u64;
    if level > 0 {
        let mut encoder = flate2::write::DeflateEncoder::new(
            Vec::with_capacity(raw.len() / 2),
            flate2::Compression::new(level),
        );
        encoder
            .write_all(&raw)
            .map_err(|error| format!("{name}: compression failed: {error}"))?;
        let compressed = encoder
            .finish()
            .map_err(|error| format!("{name}: compression failed: {error}"))?;
        if compressed.len() < raw.len() {
            return Ok(Entry {
                name,
                crc,
                size,
                method: 8,
                data: compressed,
            });
        }
    }
    Ok(Entry {
        name,
        crc,
        size,
        method: 0,
        data: raw,
    })
}

pub struct PackStats {
    pub files: usize,
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// Staging files read, for build traces.
    pub inputs: Vec<PathBuf>,
}

/// Metadata text in MakeBite's XmlSerializer layout.
pub fn metadata_text(package: &mut ModPackage) -> Result<String, String> {
    let metadata = package.meta_text.clone();
    let root = package.makebite_metadata(&metadata)?;
    let document = crate::xmltree::Doc {
        decl: "<?xml version=\"1.0\" encoding=\"utf-8\"?>".into(),
        nl: "\r\n".into(),
        root,
    };
    Ok(crate::xmltree::write(&document))
}

/// Build a .mgsv package without replacing staging inputs or sharing a temp file.
/// Compression level must be 0-9; failed writes leave the existing output intact.
pub fn write_mgsv(stage: &Path, out: &Path, level: u32) -> Result<PackStats, String> {
    if level > 9 {
        return Err("compression level must be between 0 and 9".into());
    }
    let mut package = ModPackage::open_dir(stage)?;
    if let Some(skipped) = package.skipped.first() {
        return Err(format!(
            "{} .wmv file(s) in the staging folder (never shipped): {skipped}",
            package.skipped.len()
        ));
    }

    let mut names: Vec<String> = package
        .archive_files
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    names.extend(package.loose_files.iter().map(|(name, _)| name.clone()));
    validate_zip_names(&names)?;

    let mut inputs: Vec<PathBuf> = names.iter().map(|name| stage.join(name)).collect();
    inputs.push(stage.join("metadata.xml"));
    let output = protect_staging_inputs(out, &inputs)?;
    let metadata = metadata_text(&mut package)?;

    publish_output(&output, &NEXT_OUTPUT, |file| {
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        let stats = write_zip(&mut writer, stage, &names, metadata, level, inputs)?;
        writer
            .flush()
            .map_err(|error| format!("{}: {error}", out.display()))?;
        Ok(stats)
    })
}

fn entry_count(payload_count: usize) -> Result<u16, String> {
    payload_count
        .checked_add(1)
        .filter(|&count| count < u16::MAX as usize)
        .and_then(|count| u16::try_from(count).ok())
        .ok_or_else(|| ZIP64_REQUIRED.into())
}

fn validate_zip_names(names: &[String]) -> Result<(), String> {
    entry_count(names.len())?;
    for name in names {
        if u16::try_from(name.len()).is_err() {
            return Err(format!("ZIP entry name is longer than 65535 bytes: {name}"));
        }
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase()
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn protect_staging_inputs(out: &Path, inputs: &[PathBuf]) -> Result<PathBuf, String> {
    let filename = out.file_name().ok_or("output needs a file name")?;
    let parent = out
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent
        .canonicalize()
        .map_err(|error| format!("{}: {error}", parent.display()))?;
    let output = parent.join(filename);
    let resolved = match fs::symlink_metadata(&output) {
        Ok(metadata) => {
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0
            };
            #[cfg(not(windows))]
            let redirected = metadata.file_type().is_symlink();
            if redirected || !metadata.is_file() {
                return Err(format!("output is not a regular file: {}", out.display()));
            }
            output
                .canonicalize()
                .map_err(|error| format!("{}: {error}", out.display()))?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => output,
        Err(error) => return Err(format!("{}: {error}", out.display())),
    };
    for input in inputs {
        let input = input
            .canonicalize()
            .map_err(|error| format!("{}: {error}", input.display()))?;
        if same_path(&resolved, &input) {
            return Err(format!(
                "output would replace a staging input: {}",
                out.display()
            ));
        }
    }
    Ok(resolved)
}

struct PendingOutput(Option<PathBuf>);

impl Drop for PendingOutput {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            let _ = fs::remove_file(path);
        }
    }
}

fn publish_output<T>(
    out: &Path,
    sequence: &AtomicU64,
    write: impl FnOnce(&mut File) -> Result<T, String>,
) -> Result<T, String> {
    let parent = out.parent().unwrap_or(Path::new("."));
    let (mut pending, mut file) = loop {
        let name = format!(
            ".fox-mgsv-{}-{}.tmp",
            std::process::id(),
            sequence.fetch_add(1, Ordering::Relaxed)
        );
        if out
            .file_name()
            .and_then(|filename| filename.to_str())
            .is_some_and(|filename| filename.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        let candidate = parent.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => break (PendingOutput(Some(candidate)), file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("{}: {error}", candidate.display())),
        }
    };

    let result = write(&mut file)?;
    file.sync_all()
        .map_err(|error| format!("{}: {error}", out.display()))?;
    drop(file);
    fs::rename(
        pending.0.as_ref().expect("pending output owns its path"),
        out,
    )
    .map_err(|error| format!("{}: {error}", out.display()))?;
    pending.0 = None;
    Ok(result)
}

fn zip_u32(value: u64) -> Result<u32, String> {
    // 0xffffffff denotes a ZIP64 field, rather than an ordinary classic value.
    if value >= u32::MAX as u64 {
        return Err(ZIP64_REQUIRED.into());
    }
    Ok(value as u32)
}

fn write_entry(
    writer: &mut impl Write,
    entry: Entry,
    offset: &mut u64,
) -> Result<DirectoryEntry, String> {
    let compressed_size = zip_u32(entry.data.len() as u64)?;
    let size = zip_u32(entry.size)?;
    let location = zip_u32(*offset)?;
    let name_length =
        u16::try_from(entry.name.len()).map_err(|_| "ZIP entry name is longer than 65535 bytes")?;
    let next_offset = offset
        .checked_add(30 + entry.name.len() as u64 + entry.data.len() as u64)
        .ok_or(ZIP64_REQUIRED)?;
    zip_u32(next_offset)?;

    let mut header = Vec::with_capacity(30 + entry.name.len());
    header.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
    header.extend_from_slice(&20u16.to_le_bytes());
    header.extend_from_slice(&0x0800u16.to_le_bytes());
    header.extend_from_slice(&entry.method.to_le_bytes());
    header.extend_from_slice(&DOS_TIME.to_le_bytes());
    header.extend_from_slice(&DOS_DATE.to_le_bytes());
    header.extend_from_slice(&entry.crc.to_le_bytes());
    header.extend_from_slice(&compressed_size.to_le_bytes());
    header.extend_from_slice(&size.to_le_bytes());
    header.extend_from_slice(&name_length.to_le_bytes());
    header.extend_from_slice(&0u16.to_le_bytes());
    header.extend_from_slice(entry.name.as_bytes());
    writer
        .write_all(&header)
        .map_err(|error| error.to_string())?;
    writer
        .write_all(&entry.data)
        .map_err(|error| error.to_string())?;
    *offset = next_offset;
    Ok(DirectoryEntry {
        name: entry.name,
        crc: entry.crc,
        compressed_size,
        size,
        method: entry.method,
        offset: location,
    })
}

fn write_zip(
    writer: &mut impl Write,
    stage: &Path,
    names: &[String],
    metadata: String,
    level: u32,
    inputs: Vec<PathBuf>,
) -> Result<PackStats, String> {
    let mut offset = 0;
    let first = pack_entry("metadata.xml".into(), metadata.into_bytes(), level)?;
    let mut bytes_in = first.size;
    let mut directory = vec![write_entry(writer, first, &mut offset)?];

    // Limit resident payload memory without changing the sorted staging order.
    for chunk in names.chunks(256) {
        let packed: Vec<Result<Entry, String>> = chunk
            .par_iter()
            .map(|name| {
                let raw = fs::read(stage.join(name)).map_err(|error| format!("{name}: {error}"))?;
                pack_entry(name.clone(), raw, level)
            })
            .collect();
        for entry in packed {
            let entry = entry?;
            bytes_in += entry.size;
            directory.push(write_entry(writer, entry, &mut offset)?);
        }
    }

    let directory_start = zip_u32(offset)?;
    let mut central = Vec::new();
    for entry in &directory {
        let name_length = u16::try_from(entry.name.len())
            .map_err(|_| "ZIP entry name is longer than 65535 bytes")?;
        central.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
        central.extend_from_slice(&20u16.to_le_bytes()); // MS-DOS, ZIP 2.0
        central.extend_from_slice(&20u16.to_le_bytes());
        central.extend_from_slice(&0x0800u16.to_le_bytes());
        central.extend_from_slice(&entry.method.to_le_bytes());
        central.extend_from_slice(&DOS_TIME.to_le_bytes());
        central.extend_from_slice(&DOS_DATE.to_le_bytes());
        central.extend_from_slice(&entry.crc.to_le_bytes());
        central.extend_from_slice(&entry.compressed_size.to_le_bytes());
        central.extend_from_slice(&entry.size.to_le_bytes());
        central.extend_from_slice(&name_length.to_le_bytes());
        central.extend_from_slice(&0u16.to_le_bytes()); // extra
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attributes
        central.extend_from_slice(&32u32.to_le_bytes()); // FILE_ATTRIBUTE_ARCHIVE
        central.extend_from_slice(&entry.offset.to_le_bytes());
        central.extend_from_slice(entry.name.as_bytes());
    }
    let directory_size = zip_u32(central.len() as u64)?;
    let bytes_out = offset
        .checked_add(central.len() as u64)
        .and_then(|size| size.checked_add(22))
        .ok_or(ZIP64_REQUIRED)?;
    zip_u32(bytes_out)?;
    writer
        .write_all(&central)
        .map_err(|error| error.to_string())?;

    let count = entry_count(names.len())?;
    let mut end = Vec::with_capacity(22);
    end.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    end.extend_from_slice(&0u16.to_le_bytes());
    end.extend_from_slice(&0u16.to_le_bytes());
    end.extend_from_slice(&count.to_le_bytes());
    end.extend_from_slice(&count.to_le_bytes());
    end.extend_from_slice(&directory_size.to_le_bytes());
    end.extend_from_slice(&directory_start.to_le_bytes());
    end.extend_from_slice(&0u16.to_le_bytes());
    writer.write_all(&end).map_err(|error| error.to_string())?;
    Ok(PackStats {
        files: directory.len(),
        bytes_in,
        bytes_out,
        inputs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_zip_counts_and_name_lengths_are_checked() {
        assert_eq!(entry_count(65_533).unwrap(), 65_534);
        assert!(entry_count(65_534).is_err());
        assert!(entry_count(65_535).is_err());
        assert!(entry_count(usize::MAX).is_err());
        assert!(validate_zip_names(&["a".repeat(65_535)]).is_ok());
        assert!(validate_zip_names(&["a".repeat(65_536)]).is_err());
        assert!(zip_u32(u32::MAX as u64 - 1).is_ok());
        assert!(zip_u32(u32::MAX as u64).is_err());
    }

    #[test]
    fn failed_writer_preserves_output_and_cleans_only_its_temporary_file() {
        let directory =
            std::env::temp_dir().join(format!("fox-mgsv-write-failure-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let output = directory.join("package.mgsv");
        fs::write(&output, b"original output").unwrap();
        let unrelated = output.with_extension("mgsv.foxtmp");
        fs::write(&unrelated, b"another operation").unwrap();

        let result = publish_output(&output, &AtomicU64::new(0), |file| {
            file.write_all(b"partial package").unwrap();
            Err::<(), _>("injected disk write failure".into())
        });
        assert!(result.is_err());
        assert_eq!(fs::read(&output).unwrap(), b"original output");
        assert_eq!(fs::read(&unrelated).unwrap(), b"another operation");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn temporary_namespace_destination_is_retained_without_early_publication() {
        let directory =
            std::env::temp_dir().join(format!("fox-mgsv-name-test-{}", std::process::id()));
        fs::create_dir(&directory).unwrap();
        let output = directory.join(format!(".fox-mgsv-{}-0.tmp", std::process::id()));
        let occupied = directory.join(format!(".fox-mgsv-{}-1.tmp", std::process::id()));
        fs::write(&occupied, b"unowned temporary bytes").unwrap();
        publish_output(&output, &AtomicU64::new(0), |file| {
            assert!(!output.exists(), "output was published before completion");
            file.write_all(b"completed package")
                .map_err(|error| error.to_string())
        })
        .unwrap();
        assert_eq!(fs::read(&output).unwrap(), b"completed package");
        assert_eq!(fs::read(&occupied).unwrap(), b"unowned temporary bytes");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
        fs::remove_dir_all(directory).unwrap();
    }
}
