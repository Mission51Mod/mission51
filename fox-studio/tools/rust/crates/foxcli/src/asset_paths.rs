//! Resolve virtual archive names inside the user's selected folder.
use std::path::{Path, PathBuf};

pub fn plan(root: &Path, names: &[&str]) -> Result<Vec<PathBuf>, String> {
    let paths = names
        .iter()
        .map(|name| within(root, name))
        .collect::<Result<Vec<_>, _>>()?;
    let mut keys = std::collections::BTreeSet::new();
    for path in &paths {
        let key = path.to_string_lossy().replace('\\', "/").to_lowercase();
        if !keys.insert(key) {
            return Err(format!(
                "archive entries resolve to the same output: {}",
                path.display()
            ));
        }
    }
    // Every entry is a file. A second file cannot also occupy one of its parents.
    for key in &keys {
        for (separator, _) in key.match_indices('/') {
            if keys.contains(&key[..separator]) {
                return Err(format!("archive file is also an output directory: {key}"));
            }
        }
    }
    Ok(paths)
}

/// Validate all destinations before the first extraction write, including sidecars.
/// Atomic publication separately preserves other hard links to an input file.
pub fn protect_inputs(outputs: &[PathBuf], inputs: &[&Path]) -> Result<(), String> {
    let protected = inputs
        .iter()
        .map(|input| {
            input
                .canonicalize()
                .map(|path| path.to_string_lossy().to_lowercase())
                .map_err(|error| format!("{}: {error}", input.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for output in outputs {
        reject_link(output)?;
        let resolved = resolved_output(output)?;
        if protected.contains(&resolved.to_string_lossy().to_lowercase()) {
            return Err(format!(
                "output would replace an input file: {}",
                output.display()
            ));
        }
        match std::fs::metadata(output) {
            Ok(metadata) if !metadata.is_file() => {
                return Err(format!(
                    "output is not a regular file: {}",
                    output.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", output.display())),
        }
    }
    Ok(())
}

pub fn within(root: &Path, name: &str) -> Result<PathBuf, String> {
    let normalized = name.replace('\\', "/");
    if normalized.starts_with("//") || normalized.contains([':', '\0']) {
        return Err(format!("unsupported absolute archive path {name:?}"));
    }
    // Fox asset names conventionally begin with one slash. That slash denotes
    // the archive root, not the host filesystem root.
    let relative = normalized.strip_prefix('/').unwrap_or(&normalized);
    if relative.is_empty() {
        return Err("an archive entry has an empty path".into());
    }
    let mut path = root.to_path_buf();
    reject_link(&path)?;
    for part in relative.split('/') {
        if part.is_empty() || matches!(part, "." | "..") || part.ends_with([' ', '.']) {
            return Err(format!("unsafe archive path {name:?}"));
        }
        let stem = part
            .split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_uppercase();
        let device_name = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|prefix| {
                stem.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix.len() == 1 && matches!(suffix.as_bytes()[0], b'1'..=b'9')
                })
            });
        if device_name
            || part.contains(['<', '>', '"', '|', '?', '*'])
            || part.chars().any(char::is_control)
        {
            return Err(format!("unsupported archive filename {part:?}"));
        }
        path.push(part);
        reject_link(&path)?;
    }
    Ok(path)
}

fn reject_link(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            #[cfg(windows)]
            let redirected = {
                use std::os::windows::fs::MetadataExt;
                metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
            };
            #[cfg(not(windows))]
            let redirected = metadata.file_type().is_symlink();
            if redirected {
                return Err(format!(
                    "archive path crosses a filesystem link: {}",
                    path.display()
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// Resolve an output that may not exist yet through its nearest existing parent.
/// This catches a symlinked cache folder before comparing it with the install.
pub fn resolved_output(path: &Path) -> Result<PathBuf, String> {
    let absolute = std::path::absolute(path).map_err(|error| error.to_string())?;
    let mut parent = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match parent.canonicalize() {
            Ok(mut resolved) => {
                for name in missing.iter().rev() {
                    resolved.push(name);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(
                    parent
                        .file_name()
                        .ok_or("output has no existing parent")?
                        .to_owned(),
                );
                parent = parent.parent().ok_or("output has no existing parent")?;
            }
            Err(error) => return Err(format!("{}: {error}", parent.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_asset_root_is_relative_to_the_selected_folder() {
        let root = Path::new("nonexistent-test-output");
        assert_eq!(
            within(root, "/Assets/tpp/model.fmdl").unwrap(),
            root.join("Assets/tpp/model.fmdl")
        );
        assert_eq!(
            within(root, "Assets\\tpp\\model.fmdl").unwrap(),
            root.join("Assets/tpp/model.fmdl")
        );
    }

    #[test]
    fn traversal_and_windows_special_paths_are_rejected_on_every_platform() {
        for path in [
            "../outside",
            "Assets/../../outside",
            "C:\\outside",
            "\\\\host\\share",
            "/",
            "a//b",
            "a/./b",
            "a/NUL.txt",
            "a/com1",
            "a/trailing.",
            "a/file:stream",
        ] {
            assert!(
                within(Path::new("output"), path).is_err(),
                "accepted {path:?}"
            );
        }
    }

    #[test]
    fn case_and_separator_aliases_cannot_overwrite_each_other() {
        assert!(plan(Path::new("output"), &["/Assets/one.txt", "assets\\ONE.txt"]).is_err());
    }
}
