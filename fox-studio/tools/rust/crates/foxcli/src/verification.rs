//! Read-only format and installer comparisons used by `fox` diagnostics.
use crate::{arg_value, die, runtime};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn run(args: &[String], profile_path: Option<&Path>) {
    let a = args;
    match a.first().map(String::as_str) {
        Some("sb-check") => {
            // snakebite.xml: rebuild from its parsed parts (structure proof) and recompute the merged packs' vanilla
            // FpkEntries from extracted vanilla packs (--vanilla-dirs) to learn SnakeBite's rule
            use foxinstall::snakebite as sb;
            let p = a
                .get(1)
                .unwrap_or_else(|| die("sb-check snakebite.xml [--vanilla-dirs A;B]"));
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| die(e));
            let st = sb::SbState::read(&text).unwrap_or_else(|e| die(e));
            let merged = st.merged();
            let mods: Vec<foxinstall::xmltree::Elem> = st.mods().into_iter().cloned().collect();
            let dh = st
                .game_data()
                .and_then(|g| g.attr("DatHash"))
                .unwrap_or("")
                .to_string();
            let out = foxinstall::xmltree::write(&sb::build(&st, &dh, &merged, &mods));
            if out == text {
                println!(
                    "rebuild from parts: IDENTICAL ({} merged packs, {} mods)",
                    merged.len(),
                    mods.len()
                );
            } else {
                let i = out
                    .bytes()
                    .zip(text.bytes())
                    .position(|(x, y)| x != y)
                    .unwrap_or(out.len().min(text.len()));
                println!(
                    "rebuild from parts: DIFFERENT at {i}: ours {:?} | file {:?}",
                    excerpt(&out, i, 60),
                    excerpt(&text, i, 60)
                );
            }
            if let Some(v) = arg_value(a, "--vanilla-dirs") {
                let roots: Vec<PathBuf> = v.split(';').map(PathBuf::from).collect();
                let mut files: HashMap<String, PathBuf> = HashMap::new();
                for m in &merged {
                    for r in &roots {
                        let f = r.join(m.path.trim_start_matches('/'));
                        if f.exists() {
                            files.insert(m.path.clone(), f);
                            break;
                        }
                    }
                }
                let (mut same, mut diff) = (0, 0);
                for m in &merged {
                    let Some(f) = files.get(&m.path) else {
                        println!("  no vanilla file for {}", m.path);
                        continue;
                    };
                    let b = std::fs::read(f)
                        .unwrap_or_else(|error| die(format!("{}: {error}", f.display())));
                    let pk = foxcore::fpk::read(&b).unwrap_or_else(|e| die(e));
                    let all: Vec<String> = pk
                        .entries
                        .iter()
                        .map(|e| sb::sb_inner_path(&e.path))
                        .collect();
                    // mods' inner files for this pack
                    let modded: std::collections::HashSet<String> = mods
                        .iter()
                        .flat_map(|me| {
                            me.child("FpkEntries")
                                .map(|f| {
                                    f.children
                                        .iter()
                                        .filter(|e| e.attr("FpkFile") == Some(m.path.as_str()))
                                        .map(|e| {
                                            sb::sb_inner_path(e.attr("FilePath").unwrap_or(""))
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default()
                        })
                        .collect();
                    let not_modded: Vec<String> = all
                        .iter()
                        .filter(|p| !modded.contains(*p))
                        .cloned()
                        .collect();
                    if m.vanilla_entries == all {
                        same += 1;
                        println!("  {}: all {} vanilla entries", m.path, all.len());
                    } else if m.vanilla_entries == not_modded {
                        same += 1;
                        println!(
                            "  {}: vanilla entries not replaced by a mod ({} of {})",
                            m.path,
                            not_modded.len(),
                            all.len()
                        );
                    } else {
                        diff += 1;
                        println!(
                            "  {}: recorded {} vs vanilla {} / not-modded {} - rule unknown",
                            m.path,
                            m.vanilla_entries.len(),
                            all.len(),
                            not_modded.len()
                        );
                    }
                }
                println!("merged packs explained: {same}, unexplained: {diff}");
            }
        }
        Some("roundtrip") => {
            let runtime = if a.get(1).is_some_and(|kind| kind == "fpk") {
                Some(
                    runtime::RuntimeAccess::load(profile_path, None)
                        .unwrap_or_else(|error| die(error)),
                )
            } else {
                None
            };
            // fox roundtrip <pftxs|sbp|fpk|fox2|mtar> <dir>: read + write every such file under dir; byte-compare
            let kind = a
                .get(1)
                .cloned()
                .unwrap_or_else(|| die("roundtrip <pftxs|sbp|fpk|fox2> <dir>"));
            let dir = a.get(2).cloned().unwrap_or_else(|| die("missing <dir>"));
            let exts: Vec<&str> = match kind.as_str() {
                "pftxs" => vec![".pftxs"],
                "sbp" => vec![".sbp"],
                "fpk" => vec![".fpk", ".fpkd"],
                "fox2" => vec![".fox2"],
                "mtar" => vec![".mtar"],
                _ => die("kind: pftxs | sbp | fpk | fox2 | mtar"),
            };
            let mut stack = vec![PathBuf::from(&dir)];
            let (mut n, mut same, mut bytes) = (0u64, 0u64, 0u64);
            let mut bad: Vec<String> = vec![];
            let t = std::time::Instant::now();
            while let Some(d) = stack.pop() {
                let rd = std::fs::read_dir(&d)
                    .unwrap_or_else(|error| die(format!("{}: {error}", d.display())));
                for entry in rd {
                    let entry = entry.unwrap_or_else(|error| die(error));
                    let p = entry.path();
                    let file_type = entry.file_type().unwrap_or_else(|error| die(error));
                    if file_type.is_symlink() {
                        continue;
                    }
                    if file_type.is_dir() {
                        stack.push(p);
                        continue;
                    }
                    let name = p.to_string_lossy().to_lowercase();
                    if !exts.iter().any(|x| name.ends_with(x)) {
                        continue;
                    }
                    let b = std::fs::read(&p)
                        .unwrap_or_else(|error| die(format!("{}: {error}", p.display())));
                    n += 1;
                    bytes += b.len() as u64;
                    let out: Result<Vec<u8>, String> = match kind.as_str() {
                        "pftxs" => foxcore::containers::pftxs_read(&b)
                            .map(|x| foxcore::containers::pftxs_write(&x)),
                        "sbp" => foxcore::containers::sbp_read(&b)
                            .and_then(|x| foxcore::containers::sbp_write(&x)),
                        "fpk" => foxcore::fpk::read(&b).and_then(|x| {
                            let ents: Vec<(&str, &[u8])> = x
                                .entries
                                .iter()
                                .map(|e| {
                                    (
                                        e.path.as_str(),
                                        &b[e.offset as usize..(e.offset + e.size) as usize],
                                    )
                                })
                                .collect();
                            let refs: Vec<&str> = x.references.iter().map(|s| s.as_str()).collect();
                            runtime
                                .as_ref()
                                .expect("FPK runtime selected above")
                                .order
                                .write(x.kind, &ents, &refs)
                        }),
                        "mtar" => foxcore::mtar::Mtar::read(&b).and_then(|x| x.write()),
                        _ => foxcore::fox2::read(&b).and_then(|x| foxcore::fox2::write(&x)),
                    };
                    match out {
                        Ok(o) if o == b => same += 1,
                        Ok(_) => bad.push(format!("differs: {}", p.display())),
                        Err(x) => bad.push(format!("error: {} ({x})", p.display())),
                    }
                }
            }
            println!(
                "{kind} roundtrip: {n} files ({:.1} MB), {same} byte-identical, {} not ({:.1} s)",
                bytes as f64 / 1e6,
                bad.len(),
                t.elapsed().as_secs_f64()
            );
            for x in bad.iter().take(8) {
                println!("  {x}");
            }
            if !bad.is_empty() {
                std::process::exit(1);
            }
        }
        Some("sb-modentry") => {
            // the ModEntry we would write for a package vs the one SnakeBite wrote (same mod in snakebite.xml)
            let pkg = foxinstall::mgsv::ModPackage::open(Path::new(
                a.get(1).unwrap_or_else(|| die("sb-modentry PKG XML")),
            ))
            .unwrap_or_else(|e| die(e));
            let text =
                std::fs::read_to_string(a.get(2).unwrap_or_else(|| die("missing snakebite.xml")))
                    .unwrap_or_else(|e| die(e));
            let st = foxinstall::snakebite::SbState::read(&text).unwrap_or_else(|e| die(e));
            let theirs = st
                .mods()
                .into_iter()
                .find(|m| m.attr("Name") == Some(pkg.name.as_str()))
                .unwrap_or_else(|| die("mod not in snakebite.xml"));
            let ours = foxinstall::xmltree::parse_elem(pkg.mod_entry_xml.as_deref().unwrap_or(""))
                .unwrap_or_else(|e| die(e));
            let (x, y) = (
                foxinstall::xmltree::write_elem_at(&ours, 2, "\r\n"),
                foxinstall::xmltree::write_elem_at(theirs, 2, "\r\n"),
            );
            if x == y {
                println!("ModEntry '{}': IDENTICAL ({} bytes)", pkg.name, x.len());
            } else {
                let i = x
                    .bytes()
                    .zip(y.bytes())
                    .position(|(p, q)| p != q)
                    .unwrap_or(x.len().min(y.len()));
                println!(
                    "ModEntry '{}': DIFFERENT at {i}: ours {:?} | snakebite {:?}",
                    pkg.name,
                    excerpt(&x, i, 50),
                    excerpt(&y, i, 50)
                );
            }
        }
        #[cfg(feature = "internal-pipeline")]
        Some("npy-roundtrip") => {
            let (mut n, mut same) = (0, 0);
            for f in &a[1..] {
                let b = std::fs::read(f).unwrap_or_else(|e| die(e));
                n += 1;
                match foxgeo::npy::Npy::read(&b) {
                    Ok(x) if x.write() == b => same += 1,
                    Ok(x) => println!("  differs: {f} ({} {:?})", x.descr, x.shape),
                    Err(e) => println!("  error: {f}: {e}"),
                }
            }
            println!("npy roundtrip: {n} files, {same} byte-identical");
        }
        Some("xml-roundtrip") => {
            let p = a.get(1).unwrap_or_else(|| die("xml-roundtrip FILE"));
            let text = std::fs::read_to_string(p).unwrap_or_else(|e| die(e));
            let d = foxinstall::xmltree::parse(&text).unwrap_or_else(|e| die(e));
            let out = foxinstall::xmltree::write(&d);
            if out == text {
                println!("IDENTICAL ({} bytes)", text.len());
            } else {
                let i = out
                    .bytes()
                    .zip(text.bytes())
                    .position(|(x, y)| x != y)
                    .unwrap_or(out.len().min(text.len()));
                println!(
                    "DIFFERENT at byte {i}: ours {:?} | file {:?}",
                    excerpt(&out, i, 40),
                    excerpt(&text, i, 40)
                );
                std::process::exit(1);
            }
        }
        _ => die("this diagnostic is not available in this build"),
    }
}

/// Diagnostic byte offsets can fall inside a UTF-8 character.
fn excerpt(text: &str, offset: usize, radius: usize) -> String {
    let bytes = text.as_bytes();
    let start = offset.saturating_sub(radius).min(bytes.len());
    let end = offset.saturating_add(radius).min(bytes.len());
    String::from_utf8_lossy(&bytes[start..end]).into_owned()
}
