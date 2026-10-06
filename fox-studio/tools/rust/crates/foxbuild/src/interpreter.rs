//! Canonical interpreter identity for internal authoring graphs. Portable native
//! contexts do not call this module.
use crate::cancel::Cancellation;
use crate::process;
use anyhow::Result;
use std::collections::BTreeMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};

pub(crate) fn identity(spec: &str) -> String {
    identity_cancellable(spec, &Cancellation::default())
        .ok()
        .flatten()
        .unwrap_or_else(|| format!("unresolved:{spec}"))
}

pub(crate) fn identity_cancellable(spec: &str, cancel: &Cancellation) -> Result<Option<String>> {
    if cancel.requested()? {
        return Ok(None);
    }
    static CACHE: OnceLock<Mutex<BTreeMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));
    if let Some(identity) = cache.lock().ok().and_then(|cache| cache.get(spec).cloned()) {
        return Ok(Some(identity));
    }
    let probe = "import os,sys;print(os.path.realpath(sys.executable));print(sys.version.split()[0]);print(sys.implementation.name)";
    let mut command = Command::new(spec);
    command.args(["-I", "-c", probe]);
    let identity = match process::capture(&mut command, cancel) {
        Ok(None) => return Ok(None),
        Ok(Some(output)) if output.status.success() => parse_identity(spec, &output.stdout),
        Ok(Some(_)) | Err(_) => format!("unresolved:{spec}"),
    };
    if let Ok(mut cache) = cache.lock() {
        cache.insert(spec.to_owned(), identity.clone());
    }
    Ok(Some(identity))
}

fn parse_identity(spec: &str, bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let lines: Vec<_> = text.lines().map(str::trim).collect();
    if lines.len() < 3 {
        return format!("unresolved:{spec}");
    }
    let path = if cfg!(windows) {
        lines[0].replace('/', "\\").to_lowercase()
    } else {
        lines[0].to_owned()
    };
    format!("{}|{} {}", path, lines[2], lines[1])
}
