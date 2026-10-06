//! SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Rust build-script helper for Fox Studio's Windows icon/version.
//! The GUI owner can call `embed()` from its build.rs; Linux needs no resource compiler.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn resource_compiler() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os("FOX_RC") {
        let path = PathBuf::from(path);
        return path
            .is_file()
            .then_some(path)
            .ok_or_else(|| "FOX_RC does not name an existing resource compiler".into());
    }
    let program_files = env::var_os("ProgramFiles(x86)").ok_or_else(|| {
        "Windows SDK location unavailable; set FOX_RC to Microsoft's rc.exe".to_string()
    })?;
    let base = PathBuf::from(program_files).join("Windows Kits/10/bin");
    let entries =
        fs::read_dir(&base).map_err(|error| format!("cannot inspect Windows SDK: {error}"))?;
    let mut versions: Vec<_> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions.reverse();
    versions
        .into_iter()
        .map(|path| path.join("x64/rc.exe"))
        .find(|path| path.is_file())
        .ok_or_else(|| "Microsoft Windows SDK rc.exe not found; set FOX_RC".into())
}

fn quote_resource_path(path: &Path) -> Result<String, String> {
    let text = path
        .to_str()
        .ok_or_else(|| "resource asset path is not UTF-8".to_string())?;
    if text.contains('"') || text.contains('\n') || text.contains('\r') {
        return Err("resource asset path contains invalid characters".into());
    }
    Ok(text.replace('\\', "/"))
}

pub fn embed() -> Result<(), String> {
    println!("cargo:rerun-if-env-changed=FOX_RC");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return Ok(());
    }
    let manifest = env::var_os("CARGO_MANIFEST_DIR").ok_or("missing Cargo manifest directory")?;
    let assets = PathBuf::from(manifest).join("../../../release");
    let icon = assets
        .join("fox-studio.ico")
        .canonicalize()
        .map_err(|error| format!("Fox Studio icon unavailable: {error}"))?;
    println!("cargo:rerun-if-changed={}", icon.display());
    let output = PathBuf::from(env::var_os("OUT_DIR").ok_or("missing Cargo output directory")?);
    let version = env::var("CARGO_PKG_VERSION").map_err(|error| error.to_string())?;
    let components: Vec<_> = version
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<_, _>>()
        .map_err(|_| "Windows version resource requires a numeric major.minor.patch".to_string())?;
    if components.len() != 3 {
        return Err("Windows version resource requires three version components".into());
    }
    let numeric = format!("{},{},{},0", components[0], components[1], components[2]);
    let icon_path = quote_resource_path(&icon)?;
    let source = format!(
        r#"1 ICON "{icon_path}"
1 VERSIONINFO
 FILEVERSION {numeric}
 PRODUCTVERSION {numeric}
 FILEFLAGS 0
 FILEOS 0x40004
 FILETYPE 1
 FILESUBTYPE 0
BEGIN
 BLOCK "StringFileInfo"
 BEGIN
  BLOCK "040904b0"
  BEGIN
   VALUE "CompanyName", "The Fox Engine tooling authors\0"
   VALUE "FileDescription", "Fox Studio\0"
   VALUE "FileVersion", "{version}\0"
   VALUE "InternalName", "fox-studio\0"
   VALUE "LegalCopyright", "Copyright the Fox Engine tooling authors\0"
   VALUE "OriginalFilename", "fox-studio.exe\0"
   VALUE "ProductName", "Fox Studio\0"
   VALUE "ProductVersion", "{version}\0"
  END
 END
 BLOCK "VarFileInfo"
 BEGIN
  VALUE "Translation", 0x409, 1200
 END
END
"#
    );
    let script = output.join("fox-studio.rc");
    let resource = output.join("fox-studio.res");
    fs::write(&script, source)
        .map_err(|error| format!("cannot write version resource: {error}"))?;
    let compiler = resource_compiler()?;
    let result = Command::new(compiler)
        .args(["/nologo", "/fo"])
        .arg(&resource)
        .arg(&script)
        .output()
        .map_err(|error| format!("cannot launch resource compiler: {error}"))?;
    if !result.status.success() {
        return Err(format!(
            "resource compiler failed: {}",
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    println!("cargo:rustc-link-arg={}", resource.display());
    Ok(())
}
