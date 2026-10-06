//! `fox unpack` / `fox pack`: folder + definition JSON <-> fpk/fpkd, pftxs, sbp (the GzsTool / datfpk workflow).
//!
//!   fox unpack <file> <outdir> [--dict FILE]   -> outdir/<inner files> + <outdir>.json (the definition)
//!   fox pack <definition.json> <outfile>       <- the definition's folder (the json's "folder", default: the json
//!                                                 path without ".json")
//! Definitions:
//!   fpk:   {"type": "fpk"|"fpkd", "entries": [{"filePath": ...}], "references": [{"filePath": ...}]}
//!          (datfpk's format: the same JSON packs with datfpk too)
//!   pftxs: {"type": "pftxs", "head": [..3], "texlUnknown": n, "blocks": [{"hash": "hex", "entries": [{"hash": "hex",
//!          "file": relative path}]}]}
//!   sbp:   {"type": "sbp", "headerPad": n, "entries": [{"tag": "bnk", "file": relative path}]}
//! Unpack -> pack reproduces the original bytes for every vanilla file (fox roundtrip proves the codecs; the CLI
//! path adds only file I/O).
use foxcore::{containers, fpk, qar};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

fn die(msg: impl std::fmt::Display) -> ! {
    eprintln!("fox: {msg}");
    std::process::exit(2)
}

fn write_file(p: &Path, b: &[u8]) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap_or_else(|e| die(e));
    }
    crate::output::write_atomic(p, |file| file.write_all(b))
        .unwrap_or_else(|error| die(format!("{}: {error}", p.display())));
}

fn dict(p: Option<&str>) -> HashMap<u64, String> {
    let mut m = HashMap::new();
    if let Some(p) = p {
        for l in std::fs::read_to_string(p)
            .unwrap_or_else(|e| die(e))
            .lines()
        {
            let l = l.trim();
            if !l.is_empty() {
                m.entry(qar::file_hash(l)).or_insert_with(|| l.to_string());
                m.entry(qar::path_hash(l.split('.').next().unwrap_or(l)) & 0x3_FFFF_FFFF_FFFF)
                    .or_insert_with(|| l.to_string());
            }
        }
    }
    m
}

pub fn unpack(file: &str, outdir: &str, dict_path: Option<&str>) {
    let b = std::fs::read(file).unwrap_or_else(|e| die(format!("{file}: {e}")));
    let out = PathBuf::from(outdir);
    let names = dict(dict_path);
    let definition_path = PathBuf::from(format!("{}.json", outdir.trim_end_matches(['/', '\\'])));
    let mut inputs = vec![Path::new(file)];
    if let Some(dictionary) = dict_path {
        inputs.push(Path::new(dictionary));
    }
    crate::asset_paths::protect_inputs(std::slice::from_ref(&definition_path), &inputs)
        .unwrap_or_else(|error| die(error));
    let plan = |names: &[&str]| {
        let paths = crate::asset_paths::plan(&out, names)?;
        crate::asset_paths::protect_inputs(&paths, &inputs)?;
        Ok::<_, String>(paths)
    };
    let def: Value;
    if b.starts_with(b"foxfpk") {
        let p = fpk::read(&b).unwrap_or_else(|e| die(e));
        let paths = plan(
            &p.entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
        )
        .unwrap_or_else(|error| die(error));
        for entry in &p.entries {
            let end = (entry.offset as usize).checked_add(entry.size as usize);
            if end.is_none_or(|end| end > b.len()) {
                die(format!("truncated payload for {}", entry.path));
            }
        }
        for (e, path) in p.entries.iter().zip(paths) {
            write_file(&path, &b[e.offset as usize..(e.offset + e.size) as usize]);
        }
        let kind = if p.kind == fpk::Kind::Fpkd {
            "fpkd"
        } else {
            "fpk"
        };
        let mut d = json!({"type": kind, "entries": p.entries.iter().map(|e| json!({"filePath": e.path})).collect::<Vec<_>>()});
        if kind == "fpk" {
            d["references"] = json!(
                p.references
                    .iter()
                    .map(|r| json!({"filePath": r}))
                    .collect::<Vec<_>>()
            );
        }
        def = d;
    } else if b.starts_with(b"PFTX") {
        let x = containers::pftxs_read(&b).unwrap_or_else(|e| die(e));
        let planned_names: Vec<String> = x
            .blocks
            .iter()
            .enumerate()
            .flat_map(|(block_index, block)| {
                block
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(entry_index, (hash, _))| {
                        names.get(hash).cloned().unwrap_or_else(|| {
                            format!("{block_index:04}_{hash:016x}_{entry_index}.bin")
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        plan(&planned_names.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap_or_else(|error| die(error));
        let mut blocks = vec![];
        for (bi, blk) in x.blocks.iter().enumerate() {
            let mut ents = vec![];
            for (ei, (h, data)) in blk.entries.iter().enumerate() {
                let name = names
                    .get(h)
                    .cloned()
                    .unwrap_or_else(|| format!("{bi:04}_{h:016x}_{ei}.bin"));
                let rel = name.trim_start_matches('/').to_string();
                write_file(
                    &crate::asset_paths::within(&out, &rel).unwrap_or_else(|error| die(error)),
                    data,
                );
                ents.push(json!({"hash": format!("{h:016x}"), "file": rel}));
            }
            blocks.push(json!({"hash": format!("{:016x}", blk.hash), "entries": ents}));
        }
        def = json!({"type": "pftxs", "head": x.head, "texlUnknown": x.texl_unknown, "blocks": blocks});
    } else if b.starts_with(b"SBPL") {
        let x = containers::sbp_read(&b).unwrap_or_else(|e| die(e));
        let stem = Path::new(file)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        let names: Vec<String> = x
            .entries
            .iter()
            .enumerate()
            .map(|(index, (tag, _))| {
                let tag = String::from_utf8_lossy(tag);
                format!("{stem}_{index}.{}", tag.trim_end_matches('\0'))
            })
            .collect();
        let paths = plan(&names.iter().map(String::as_str).collect::<Vec<_>>())
            .unwrap_or_else(|error| die(error));
        let mut ents = vec![];
        for (((tag, data), name), path) in x.entries.iter().zip(names).zip(paths) {
            write_file(&path, data);
            let tag = String::from_utf8_lossy(tag)
                .trim_end_matches('\0')
                .to_string();
            ents.push(json!({"tag": tag, "file": name}));
        }
        def = json!({"type": "sbp", "headerPad": x.header_pad, "entries": ents});
    } else {
        die("unpack: fpk / fpkd / pftxs / sbp (use `fox qar extract` for .dat archives)");
    }
    write_file(
        &definition_path,
        serde_json::to_string_pretty(&def).unwrap().as_bytes(),
    );
    println!("{file} -> {outdir} + {}", definition_path.display());
}

/// Read a required array with a useful location in malformed-definition errors.
fn array<'a>(value: &'a Value, label: &str) -> Result<&'a [Value], String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| format!("{label} must be an array"))
}

fn text<'a>(value: &'a Value, label: &str) -> Result<&'a str, String> {
    value
        .as_str()
        .filter(|text| !text.is_empty())
        .ok_or_else(|| format!("{label} must be a nonempty string"))
}

fn number(value: &Value, label: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .ok_or_else(|| format!("{label} must be an unsigned 32-bit integer"))
}

fn hex(value: &Value, label: &str) -> Result<u64, String> {
    u64::from_str_radix(text(value, label)?, 16).map_err(|error| format!("{label}: {error}"))
}

fn read_input(folder: &Path, relative: &str) -> Result<Vec<u8>, String> {
    let path = crate::asset_paths::within(folder, relative)?;
    std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))
}

fn build_definition(
    definition: &Value,
    folder: &Path,
    profile_path: Option<&Path>,
) -> Result<Vec<u8>, String> {
    match text(&definition["type"], "type")? {
        kind_name @ ("fpk" | "fpkd") => {
            let kind = if kind_name == "fpk" {
                fpk::Kind::Fpk
            } else {
                fpk::Kind::Fpkd
            };
            let entries = array(&definition["entries"], "entries")?;
            let paths = entries
                .iter()
                .map(|entry| text(&entry["filePath"], "entry.filePath"))
                .collect::<Result<Vec<_>, _>>()?;
            let references = match definition.get("references") {
                Some(value) => array(value, "references")?
                    .iter()
                    .map(|entry| text(&entry["filePath"], "reference.filePath"))
                    .collect::<Result<Vec<_>, _>>()?,
                None => Vec::new(),
            };
            crate::asset_paths::plan(folder, &paths)?;
            let data = paths
                .iter()
                .map(|path| read_input(folder, path))
                .collect::<Result<Vec<_>, _>>()?;
            let entries = paths
                .iter()
                .zip(&data)
                .map(|(path, bytes)| (*path, bytes.as_slice()))
                .collect::<Vec<_>>();
            let runtime = crate::runtime::RuntimeAccess::load(profile_path, None)?;
            runtime.order.write(kind, &entries, &references)
        }
        "pftxs" => {
            let values = array(&definition["head"], "head")?;
            if values.len() != 3 {
                return Err("head must contain exactly three unsigned integers".into());
            }
            let head = [
                number(&values[0], "head[0]")?,
                number(&values[1], "head[1]")?,
                number(&values[2], "head[2]")?,
            ];
            let mut blocks = Vec::new();
            for block in array(&definition["blocks"], "blocks")? {
                let hash = hex(&block["hash"], "block.hash")?;
                let mut entries = Vec::new();
                for entry in array(&block["entries"], "block.entries")? {
                    entries.push((
                        hex(&entry["hash"], "entry.hash")?,
                        read_input(folder, text(&entry["file"], "entry.file")?)?,
                    ));
                }
                blocks.push(containers::FtexBlock { hash, entries });
            }
            let texl_unknown = definition
                .get("texlUnknown")
                .map(|value| number(value, "texlUnknown"))
                .transpose()?
                .unwrap_or(0);
            Ok(containers::pftxs_write(&containers::Pftxs {
                head,
                texl_unknown,
                blocks,
            }))
        }
        "sbp" => {
            let source_entries = array(&definition["entries"], "entries")?;
            if source_entries.len() > u8::MAX as usize {
                return Err("an SBP sound package can contain at most 255 entries".into());
            }
            let mut entries = Vec::new();
            for entry in source_entries {
                let name = text(&entry["tag"], "entry.tag")?;
                if name.is_empty() || !name.is_ascii() || name.len() > 4 {
                    return Err("entry.tag must contain one to four ASCII bytes".into());
                }
                let mut tag = [0; 4];
                tag[..name.len()].copy_from_slice(name.as_bytes());
                entries.push((
                    tag,
                    read_input(folder, text(&entry["file"], "entry.file")?)?,
                ));
            }
            let padding = definition
                .get("headerPad")
                .map(|value| number(value, "headerPad"))
                .transpose()?
                .unwrap_or(0);
            let header_pad = u8::try_from(padding).map_err(|_| "headerPad must fit in one byte")?;
            containers::sbp_write(&containers::Sbp {
                header_pad,
                entries,
            })
        }
        kind => Err(format!("unsupported package type {kind:?}")),
    }
}

pub fn pack(profile_path: Option<&Path>, def_path: &str, outfile: &str) {
    let definition_path = Path::new(def_path);
    let bytes =
        std::fs::read(definition_path).unwrap_or_else(|error| die(format!("{def_path}: {error}")));
    let definition: Value =
        serde_json::from_slice(&bytes).unwrap_or_else(|error| die(format!("{def_path}: {error}")));
    let folder = match definition.get("folder") {
        Some(value) => {
            let path = PathBuf::from(text(value, "folder").unwrap_or_else(|error| die(error)));
            if path.is_absolute() {
                path
            } else {
                definition_path
                    .parent()
                    .unwrap_or(Path::new("."))
                    .join(path)
            }
        }
        None => definition_path.with_extension(""),
    };
    let mut protected = vec![definition_path.to_path_buf()];
    if let Some(profile) = profile_path {
        protected.push(profile.to_path_buf());
    }
    let kind = text(&definition["type"], "type").unwrap_or_else(|error| die(error));
    let entries: Vec<&Value> = match kind {
        "pftxs" => array(&definition["blocks"], "blocks")
            .unwrap_or_else(|error| die(error))
            .iter()
            .flat_map(|block| {
                array(&block["entries"], "block.entries")
                    .unwrap_or_else(|error| die(error))
                    .iter()
            })
            .collect(),
        "fpk" | "fpkd" | "sbp" => array(&definition["entries"], "entries")
            .unwrap_or_else(|error| die(error))
            .iter()
            .collect(),
        _ => Vec::new(),
    };
    let field = if matches!(kind, "fpk" | "fpkd") {
        "filePath"
    } else {
        "file"
    };
    for entry in entries {
        let relative = text(&entry[field], field).unwrap_or_else(|error| die(error));
        protected
            .push(crate::asset_paths::within(&folder, relative).unwrap_or_else(|error| die(error)));
    }
    crate::asset_paths::protect_inputs(
        &[PathBuf::from(outfile)],
        &protected.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
    )
    .unwrap_or_else(|error| die(error));
    let output =
        build_definition(&definition, &folder, profile_path).unwrap_or_else(|error| die(error));
    write_file(Path::new(outfile), &output);
    println!("{def_path} -> {outfile} ({} bytes)", output.len());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn malformed_definitions_return_errors_instead_of_panicking() {
        for definition in [
            json!({"type":"pftxs","head":[],"blocks":[]}),
            json!({"type":"pftxs","head":[0,1,4294967296u64],"blocks":[]}),
            json!({"type":"sbp","entries":null}),
            json!({"type":"sbp","entries":[{"tag":"","file":"a"}]}),
            json!({"type":"sbp","entries":[{"tag":"long-tag","file":"a"}]}),
            json!({"type":"fpk","entries":[{}]}),
            json!({"type":"other"}),
        ] {
            assert!(
                build_definition(&definition, Path::new("unused"), None).is_err(),
                "accepted {definition}"
            );
        }
    }

    #[test]
    fn standalone_texture_container_needs_no_installation_profile() {
        let definition = json!({"type":"pftxs","head":[1,2,3],"blocks":[],"texlUnknown":4});
        let bytes = build_definition(&definition, Path::new("unused"), None).unwrap();
        let decoded = containers::pftxs_read(&bytes).unwrap();
        assert_eq!(decoded.head, [1, 2, 3]);
        assert_eq!(decoded.texl_unknown, 4);
    }
}
