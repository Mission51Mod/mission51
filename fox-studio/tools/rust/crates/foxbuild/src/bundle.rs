//! Explicit, verified native tools for a portable build. No interpreter, PATH search,
//! source checkout, process environment mutation, or tool compilation is involved.
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildIdentity {
    pub package_version: String,
    pub build_id: String,
}

impl BuildIdentity {
    /// A package build supplies one ID for its CLI, scheduler, GUI, and manifest.
    /// An ordinary development binary cannot silently adopt a packaged toolset.
    pub fn compiled() -> Result<Self> {
        let build_id = option_env!("FOX_BUNDLE_BUILD_ID")
            .context("this binary has no FOX_BUNDLE_BUILD_ID; install a matching native package")?;
        let identity = Self {
            package_version: env!("CARGO_PKG_VERSION").to_owned(),
            build_id: build_id.to_owned(),
        };
        identity.validate()?;
        Ok(identity)
    }

    fn validate(&self) -> Result<()> {
        for (field, value) in [
            ("package_version", &self.package_version),
            ("build_id", &self.build_id),
        ] {
            if value.is_empty() || value.trim() != value || value.chars().any(char::is_control) {
                bail!(
                    "bundle {field} must be nonempty text without control or surrounding whitespace"
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    package_version: String,
    build_id: String,
    tools: BTreeMap<String, ToolDefinition>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ToolDefinition {
    path: String,
    blake3: String,
}

#[derive(Debug, Clone)]
struct VerifiedTool {
    path: PathBuf,
    hash: String,
}

/// A private, immutable identity snapshot. Resolve again immediately before spawn
/// to reject executable replacement after planning. A manifest is integrity data,
/// not authentication: the caller selects the trusted package and compiled ID.
#[derive(Debug, Clone)]
pub struct BundledTools {
    root: PathBuf,
    identity: BuildIdentity,
    tools: BTreeMap<String, VerifiedTool>,
    fingerprint: String,
}

impl BundledTools {
    /// Standard packaged CLI/GUI entry point: trust the compiled identity, never
    /// the identity claimed by the manifest itself.
    pub fn load_compiled(manifest_path: &Path) -> Result<Self> {
        Self::load(manifest_path, &BuildIdentity::compiled()?)
    }

    pub fn load(manifest_path: &Path, expected: &BuildIdentity) -> Result<Self> {
        expected.validate()?;
        let manifest_path = manifest_path
            .canonicalize()
            .with_context(|| format!("opening bundle manifest {}", manifest_path.display()))?;
        let root = manifest_path
            .parent()
            .context("bundle manifest has no parent")?
            .to_owned();
        let bytes = std::fs::read(&manifest_path)?;
        let manifest: Manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing bundle manifest {}", manifest_path.display()))?;
        if manifest.schema_version != 1 {
            bail!(
                "unsupported bundle schema_version {}; expected 1",
                manifest.schema_version
            );
        }
        if manifest.package_version != expected.package_version
            || manifest.build_id != expected.build_id
        {
            bail!(
                "bundle identity mismatch: expected {} / {}, found {} / {}",
                expected.package_version,
                expected.build_id,
                manifest.package_version,
                manifest.build_id
            );
        }
        if !manifest.tools.contains_key("fox") {
            bail!("bundle must declare its native fox tool");
        }

        let mut tools = BTreeMap::new();
        let mut fingerprint = blake3::Hasher::new();
        for value in [&manifest.package_version, &manifest.build_id] {
            fingerprint.update(value.as_bytes());
            fingerprint.update(b"\0");
        }
        for (name, definition) in manifest.tools {
            if name.is_empty()
                || !name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
                || name == "py"
                || name.starts_with("python")
            {
                bail!("invalid native bundle tool name {name:?}");
            }
            if definition.blake3.len() != 64
                || !definition
                    .blake3
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                bail!("bundle tool {name} needs a full lowercase BLAKE3 hash");
            }
            let relative = definition.path.replace('\\', "/");
            if relative.contains(':')
                || relative.chars().any(char::is_control)
                || relative
                    .split('/')
                    .any(|p| p.is_empty() || p == "." || p == "..")
            {
                bail!("bundle tool {name} path must stay relative to the manifest");
            }
            let path = root
                .join(&relative)
                .canonicalize()
                .with_context(|| format!("resolving bundle tool {name}: {relative}"))?;
            verify_tool(&root, &path, &definition.blake3)
                .with_context(|| format!("validating bundle tool {name}"))?;
            for value in [&name, &relative, &definition.blake3] {
                fingerprint.update(value.as_bytes());
                fingerprint.update(b"\0");
            }
            tools.insert(
                name,
                VerifiedTool {
                    path,
                    hash: definition.blake3,
                },
            );
        }
        Ok(Self {
            root,
            identity: expected.clone(),
            tools,
            fingerprint: fingerprint.finalize().to_hex().to_string(),
        })
    }

    pub fn identity(&self) -> &BuildIdentity {
        &self.identity
    }

    /// Relocating the whole package preserves identity; changing tool bytes,
    /// relative layout, package version or build selection changes it.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn resolve_program(&self, program: &str) -> Result<&Path> {
        let alias = self.program_alias(program)?;
        let tool = &self.tools[alias];
        verify_tool(&self.root, &tool.path, &tool.hash)
            .with_context(|| format!("bundle tool {alias} changed after planning"))?;
        Ok(&tool.path)
    }

    /// Accept Q's explicit bound path only when it names an already declared
    /// executable. An arbitrary absolute path cannot select a different tool.
    pub fn program_alias(&self, program: &str) -> Result<&str> {
        if Path::new(program).is_absolute() {
            let path = Path::new(program)
                .canonicalize()
                .with_context(|| format!("resolving bound native program {program:?}"))?;
            return self.tools.iter()
                .find(|(_, tool)| tool.path == path)
                .map(|(alias, _)| alias.as_str())
                .with_context(|| format!("bound program {program:?} is not in this bundle; re-plan with its declared tool"));
        }
        let normalized = program.replace('\\', "/");
        let alias = normalized
            .strip_prefix("work/rust/target/release/")
            .unwrap_or(&normalized);
        if alias.contains('/') || alias.contains(':') {
            bail!("program {program:?} is not a declared bundle tool alias");
        }
        let alias = alias.strip_suffix(".exe").unwrap_or(alias);
        self.tools
            .get_key_value(alias)
            .map(|(alias, _)| alias.as_str())
            .with_context(|| format!("native bundle does not declare tool {alias:?}"))
    }

    pub fn verify_current(&self) -> Result<()> {
        for (alias, tool) in &self.tools {
            verify_tool(&self.root, &tool.path, &tool.hash)
                .with_context(|| format!("bundle tool {alias} changed after planning"))?;
        }
        Ok(())
    }

    /// The portable fingerprint binds native commands to this verified toolset,
    /// while alias normalization permits an unchanged package to relocate.
    pub fn command_fingerprint(&self, stage: &crate::config::Stage) -> Result<String> {
        let command = stage.run_cmd();
        let alias = self
            .program_alias(&command[0])
            .with_context(|| format!("stage {} requires a declared native tool", stage.name))?;
        // A nested scheduler would parse a new graph without inheriting this
        // immutable validated context. Refuse it rather than allowing an
        // unbundled child to start Python or arbitrary programs.
        let tool = &self.tools[alias];
        let standalone_scheduler = alias.eq_ignore_ascii_case("foxbuild")
            || tool
                .path
                .file_stem()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.eq_ignore_ascii_case("foxbuild"));
        // A declared alias may point to a renamed copy or hardlink. Verified
        // byte identity must preserve fox dispatch semantics as well as paths.
        let fox = &self.tools["fox"];
        let fox_dispatch = tool.path == fox.path || tool.hash == fox.hash;
        if standalone_scheduler || (fox_dispatch && fox_subcommand(&command[1..]) == Some("build"))
        {
            bail!(
                "stage {}: recursive build commands are unavailable in native bundled mode",
                stage.name
            );
        }
        for argument in &command[1..] {
            let basename = argument
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(argument)
                .to_ascii_lowercase();
            if basename == "py"
                || basename == "py.exe"
                || basename.starts_with("python")
                || basename.ends_with(".py")
                || basename.ends_with(".pyw")
            {
                bail!(
                    "stage {}: Python arguments are unavailable in native bundled mode: {argument:?}",
                    stage.name
                );
            }
        }
        let mut normalized = stage.clone();
        if stage.uses_rs() {
            normalized.cmd_rs.as_mut().expect("proven command exists")[0] = alias.to_owned();
        } else {
            normalized.cmd[0] = alias.to_owned();
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"foxbuild-native-bundle-v1\0");
        hash.update(self.fingerprint.as_bytes());
        hash.update(b"\0");
        hash.update(crate::cmd_fp(&normalized, "").as_bytes());
        Ok(hash.finalize().to_hex()[..16].to_owned())
    }
}

// fox's global --runtime-profile option may precede or follow its subcommand.
// Mirror only that selection rule; every other native command keeps its argv.
fn fox_subcommand(arguments: &[String]) -> Option<&str> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        if argument == "--runtime-profile" {
            index += 2;
        } else {
            return Some(argument);
        }
    }
    None
}

fn verify_tool(root: &Path, path: &Path, expected_hash: &str) -> Result<()> {
    let actual_path = path.canonicalize()?;
    if !actual_path.starts_with(root) {
        bail!("executable escapes bundle root: {}", actual_path.display());
    }
    let mut file = std::fs::File::open(&actual_path)?;
    if !file.metadata()?.is_file() {
        bail!("bundle executable is not a file: {}", actual_path.display());
    }
    let mut hash = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if hash.finalize().to_hex().as_str() != expected_hash {
        bail!("executable BLAKE3 mismatch: {}", actual_path.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Fixture(PathBuf);

    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "foxbuild_bundle_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            std::fs::create_dir(path.join("bin")).unwrap();
            std::fs::write(path.join("bin/fox.exe"), b"native fixture executable bytes").unwrap();
            Self(path)
        }

        fn identity() -> BuildIdentity {
            BuildIdentity {
                package_version: "0.1.0".into(),
                build_id: "fixture-build".into(),
            }
        }

        fn manifest(&self) -> serde_json::Value {
            serde_json::json!({
                "schema_version": 1,
                "package_version": "0.1.0",
                "build_id": "fixture-build",
                "tools": {"fox": {"path": "bin/fox.exe", "blake3":
                    blake3::hash(&std::fs::read(self.0.join("bin/fox.exe")).unwrap()).to_hex().to_string()}}
            })
        }

        fn load(&self, manifest: serde_json::Value) -> Result<BundledTools> {
            let path = self.0.join("fox-tools.json");
            std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
            BundledTools::load(&path, &Self::identity())
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn native_resolution_needs_no_source_tree_or_interpreter() {
        let fixture = Fixture::new();
        let bundle = fixture.load(fixture.manifest()).unwrap();
        let expected = fixture.0.join("bin/fox.exe").canonicalize().unwrap();
        for alias in ["fox", "fox.exe", "work/rust/target/release/fox.exe"] {
            assert_eq!(bundle.resolve_program(alias).unwrap(), expected);
        }
        assert!(!fixture.0.join("tools").exists());
        assert!(bundle.resolve_program("python").is_err());
        assert!(bundle.resolve_program("missing").is_err());
        assert!(bundle.resolve_program("../fox.exe").is_err());
    }

    #[test]
    fn identity_and_schema_must_match() {
        let fixture = Fixture::new();
        for (field, value) in [
            ("schema_version", serde_json::json!(2)),
            ("package_version", serde_json::json!("0.0.9")),
            ("build_id", serde_json::json!("stale-build")),
        ] {
            let mut manifest = fixture.manifest();
            manifest[field] = value;
            assert!(
                fixture.load(manifest).is_err(),
                "accepted mismatched {field}"
            );
        }
        let mut identity = Fixture::identity();
        identity.build_id = " ".into();
        assert!(identity.validate().is_err());
    }

    #[test]
    fn full_hash_is_required_and_tool_replacement_is_rejected() {
        let fixture = Fixture::new();
        let bundle = fixture.load(fixture.manifest()).unwrap();
        for hash in ["a".repeat(16), "A".repeat(64), "0".repeat(64)] {
            let mut manifest = fixture.manifest();
            manifest["tools"]["fox"]["blake3"] = hash.into();
            assert!(fixture.load(manifest).is_err());
        }
        std::fs::write(fixture.0.join("bin/fox.exe"), b"replacement").unwrap();
        assert!(bundle.resolve_program("fox").is_err());
    }

    #[test]
    fn absolute_parent_and_missing_paths_are_rejected() {
        let fixture = Fixture::new();
        for path in [
            "../fox.exe",
            "/fox.exe",
            "C:\\fox.exe",
            "bin/../fox.exe",
            "missing.exe",
            "bin",
        ] {
            let mut manifest = fixture.manifest();
            manifest["tools"]["fox"]["path"] = path.into();
            assert!(fixture.load(manifest).is_err(), "accepted {path}");
        }
    }

    #[test]
    fn manifest_typos_and_interpreter_tools_are_rejected() {
        let fixture = Fixture::new();
        let mut typo = fixture.manifest();
        typo["tools"]["fox"]["hash"] = "not an allowed field".into();
        assert!(fixture.load(typo).is_err());
        let mut interpreter = fixture.manifest();
        interpreter["tools"]["python"] = interpreter["tools"]["fox"].clone();
        assert!(fixture.load(interpreter).is_err());
    }

    #[test]
    fn relocating_a_package_preserves_fingerprint_but_new_bytes_do_not() {
        let first = Fixture::new();
        let second = Fixture::new();
        let a = first.load(first.manifest()).unwrap();
        let b = second.load(second.manifest()).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        std::fs::write(second.0.join("bin/fox.exe"), b"new native build").unwrap();
        let changed = second.load(second.manifest()).unwrap();
        assert_ne!(a.fingerprint(), changed.fingerprint());
    }

    #[test]
    fn review_recursive_schedulers_are_rejected_through_aliases_and_global_options() {
        let fixture = Fixture::new();
        let mut manifest = fixture.manifest();
        manifest["tools"]["other"] = manifest["tools"]["fox"].clone();
        manifest["tools"]["foxbuild"] = manifest["tools"]["fox"].clone();
        // Windows filenames may use another casing and a neutral tool alias.
        std::fs::copy(
            fixture.0.join("bin/fox.exe"),
            fixture.0.join("bin/FOXBUILD.EXE"),
        )
        .unwrap();
        manifest["tools"]["builder"] = manifest["tools"]["fox"].clone();
        manifest["tools"]["builder"]["path"] = "bin/FOXBUILD.EXE".into();
        std::fs::copy(
            fixture.0.join("bin/fox.exe"),
            fixture.0.join("bin/relay.exe"),
        )
        .unwrap();
        manifest["tools"]["copied"] = manifest["tools"]["fox"].clone();
        manifest["tools"]["copied"]["path"] = "bin/relay.exe".into();
        let bundle = fixture.load(manifest).unwrap();
        let bound = bundle
            .resolve_program("fox")
            .unwrap()
            .to_string_lossy()
            .to_string();
        for command in [
            vec!["fox".to_string(), "build".into()],
            vec!["other".to_string(), "build".into()],
            vec!["copied".to_string(), "build".into()],
            vec![
                "work/rust/target/release/fox.exe".to_string(),
                "build".into(),
            ],
            vec![bound, "build".into()],
            vec![
                "fox".to_string(),
                "--runtime-profile".into(),
                "runtime.json".into(),
                "build".into(),
                "--bundled-tools".into(),
                "different.json".into(),
            ],
            vec![
                "foxbuild".to_string(),
                "--config".into(),
                "inner.toml".into(),
            ],
            vec![
                "builder".to_string(),
                "--config".into(),
                "inner.toml".into(),
            ],
        ] {
            let mut stage: crate::config::Stage =
                toml::from_str("name = 'recursive'\ncmd = ['fox', 'pack']\n").unwrap();
            stage.cmd = command;
            assert!(
                bundle
                    .command_fingerprint(&stage)
                    .unwrap_err()
                    .to_string()
                    .contains("recursive build"),
                "accepted {:?}",
                stage.cmd
            );
        }
        let stage: crate::config::Stage =
            toml::from_str("name = 'asset'\ncmd = ['fox', 'pack', 'build']\n").unwrap();
        assert!(bundle.command_fingerprint(&stage).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_escape_is_rejected_even_with_a_matching_hash() {
        let fixture = Fixture::new();
        let outside = Fixture::new();
        std::os::unix::fs::symlink(outside.0.join("bin/fox.exe"), fixture.0.join("escape.exe"))
            .unwrap();
        let mut manifest = fixture.manifest();
        manifest["tools"]["fox"]["path"] = "escape.exe".into();
        assert!(fixture.load(manifest).is_err());
    }
}
