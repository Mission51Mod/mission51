//! QAR inspection, extraction and raw-block rebuilding.
use crate::{arg_value, die};
use foxcore::qar;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

fn open_index(context: &qar::Context, p: &str) -> (BufReader<File>, qar::Index) {
    let f = File::open(p).unwrap_or_else(|e| die(format!("{p}: {e}")));
    let mut r = BufReader::with_capacity(1 << 20, f);
    let idx = context
        .read_index(&mut r)
        .unwrap_or_else(|e| die(format!("{p}: {e}")));
    (r, idx)
}

fn stored(r: &mut BufReader<File>, e: &qar::Entry) -> Vec<u8> {
    let mut b = vec![0u8; e.stored as usize];
    r.seek(SeekFrom::Start(e.offset + 32))
        .unwrap_or_else(|x| die(x));
    r.read_exact(&mut b)
        .unwrap_or_else(|x| die(format!("entry at {}: {x}", e.offset)));
    b
}

/// path dictionary: one game path per line (without or with extension); keyed by 50-bit path hash
fn load_dict(p: Option<&str>) -> HashMap<u64, String> {
    let mut m = HashMap::new();
    if let Some(p) = p {
        let text = std::fs::read_to_string(p).unwrap_or_else(|e| die(format!("{p}: {e}")));
        for line in text.lines() {
            let l = line.trim();
            if l.is_empty() {
                continue;
            }
            let stem = l.split('.').next().unwrap_or(l);
            m.entry(qar::path_hash(stem) & 0x3_FFFF_FFFF_FFFF)
                .or_insert_with(|| stem.to_string());
        }
    }
    m
}

fn name_of(hash: u64, dict: &HashMap<u64, String>) -> (String, bool) {
    let ph = hash & 0x3_FFFF_FFFF_FFFF;
    let ext = qar::ext_of(hash);
    match (dict.get(&ph), ext) {
        (Some(s), Some(e)) => (format!("{s}.{e}"), true),
        (Some(s), None) => (format!("{s}._unknown"), false),
        (None, Some(e)) => (format!("{ph:x}.{e}"), false),
        (None, None) => (format!("{ph:x}._unknown"), false),
    }
}

fn decoded(context: &qar::Context, entry: &qar::Entry, stored: &[u8]) -> Result<Vec<u8>, String> {
    let expected = if entry.compressed() {
        entry.uncompressed as usize
    } else {
        (entry.uncompressed as usize)
            .checked_sub(context.layer2_header_len(entry, stored))
            .ok_or("decoded length is smaller than its header")?
    };
    let content = context.decode(entry, stored)?;
    if content.len() != expected {
        return Err(format!(
            "decoded length {} does not match declared {expected}",
            content.len()
        ));
    }
    Ok(content)
}

pub fn run(a: &[String], context: &qar::Context, profile_path: Option<&Path>) {
    let sub = a.first().map(|s| s.as_str()).unwrap_or("");
    let dat = a.get(1).cloned().unwrap_or_else(|| die("missing <dat>"));
    let (mut r, idx) = open_index(context, &dat);
    let h = &idx.header;
    match sub {
        "info" => {
            let comp = idx.entries.iter().filter(|e| e.compressed()).count();
            let total: u64 = idx.entries.iter().map(|e| e.stored as u64).sum();
            println!(
                "{dat}: flags {:#x} version {} entries {} extra {} data_offset {} end_block {} (block {} B)",
                h.flags,
                h.version,
                h.count,
                h.extra_count,
                h.data_offset,
                h.end_block,
                1u64 << h.block_shift
            );
            println!("compressed {comp}, stored bytes {total}");
        }
        "list" => {
            let dict = load_dict(arg_value(a, "--dict").as_deref());
            let mut out = std::io::BufWriter::new(std::io::stdout());
            for e in &idx.entries {
                let (n, _) = name_of(e.hash, &dict);
                writeln!(
                    out,
                    "{:016x} {:>12} {:>10} {:>10} {} {}",
                    e.hash,
                    e.offset,
                    e.uncompressed,
                    e.stored,
                    if e.compressed() { "z" } else { "-" },
                    n
                )
                .ok();
            }
        }
        "verify" => {
            use md5::{Digest, Md5};
            let (mut ok, mut bad, mut md5_plain, mut md5_pre, mut md5_none) = (0, 0, 0, 0, 0);
            for e in &idx.entries {
                let s = stored(&mut r, e);
                let mut l1 = s.clone();
                context.layer1(&mut l1, e.hash, 0);
                let hs = context.layer2_header_len(e, &s);
                let want = if e.compressed() {
                    e.uncompressed as usize
                } else if let Some(length) = (e.uncompressed as usize).checked_sub(hs) {
                    length
                } else {
                    bad += 1;
                    eprintln!("{:016x}: decoded length is smaller than its header", e.hash);
                    continue;
                };
                match context.decode(e, &s) {
                    Ok(c) if c.len() == want => {
                        ok += 1;
                        let m: [u8; 16] = Md5::digest(&c).into();
                        let m2: [u8; 16] = Md5::digest(&l1).into();
                        if m == e.md5 {
                            md5_plain += 1;
                        } else if m2 == e.md5 {
                            md5_pre += 1;
                        } else {
                            md5_none += 1;
                            bad += 1;
                            eprintln!(
                                "{:016x}: checksum does not match either supported representation",
                                e.hash
                            );
                        }
                    }
                    Ok(c) => {
                        bad += 1;
                        eprintln!("{:016x}: size {} != {}", e.hash, c.len(), want);
                    }
                    Err(x) => {
                        bad += 1;
                        eprintln!("{:016x}: {x}", e.hash);
                    }
                }
            }
            println!(
                "{dat}: {ok} entries decode, {bad} fail; md5 = content {md5_plain}, = stored-after-layer1 {md5_pre}, \
                      neither {md5_none}"
            );
            if bad > 0 {
                std::process::exit(1);
            }
        }
        "extract" => {
            let outdir =
                PathBuf::from(a.get(2).cloned().unwrap_or_else(|| die("missing <outdir>")));
            let dictionary_path = arg_value(a, "--dict");
            let dict = load_dict(dictionary_path.as_deref());
            let only = arg_value(a, "--only");
            let selected: Vec<_> = idx
                .entries
                .iter()
                .filter_map(|entry| {
                    let (name, _) = name_of(entry.hash, &dict);
                    if only.as_ref().is_some_and(|filter| !name.contains(filter)) {
                        return None;
                    }
                    Some((entry, name))
                })
                .collect();
            let paths = crate::asset_paths::plan(
                &outdir,
                &selected
                    .iter()
                    .map(|(_, name)| name.as_str())
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_else(|error| die(error));
            let mut inputs = vec![Path::new(&dat)];
            inputs.extend(profile_path);
            if let Some(dictionary) = &dictionary_path {
                inputs.push(std::path::Path::new(dictionary));
            }
            crate::asset_paths::protect_inputs(&paths, &inputs).unwrap_or_else(|error| die(error));
            let mut n = 0;
            for ((e, _), p) in selected.into_iter().zip(paths) {
                let c = decoded(context, e, &stored(&mut r, e))
                    .unwrap_or_else(|x| die(format!("{:016x}: {x}", e.hash)));
                if let Some(d) = p.parent() {
                    std::fs::create_dir_all(d).unwrap_or_else(|x| die(x));
                }
                crate::output::write_atomic(&p, |file| file.write_all(&c))
                    .unwrap_or_else(|error| die(format!("{}: {error}", p.display())));
                n += 1;
            }
            println!("{n} files -> {}", outdir.display());
        }
        "relayout-check" => {
            // rebuild: header + section table + (extra) + align + entries in their file order, each block-aligned
            let total = std::fs::metadata(&dat).map(|m| m.len()).unwrap_or(0);
            let shift = h.block_shift;
            let align = 1u64 << shift;
            let mut order: Vec<usize> = (0..idx.entries.len()).collect();
            order.sort_by_key(|&i| idx.entries[i].offset);
            let first = idx.entries.iter().map(|e| e.offset).min().unwrap_or(0);
            let table_end = 32 + 8 * h.count as u64 + 16 * h.extra_count as u64;
            let data_start = table_end.div_ceil(align) * align;
            let mut pos = data_start;
            let mut same_offsets = true;
            for &i in &order {
                let e = &idx.entries[i];
                if e.offset != pos {
                    same_offsets = false;
                }
                pos = (pos + e.raw_len()).div_ceil(align) * align;
            }
            println!(
                "first entry at {first} (computed {data_start}, header data_offset {}), file {total} B, computed \
                      end {pos} (end_block {} -> {}), table order == file order: {}, offsets reproduced: {}",
                h.data_offset,
                h.end_block,
                (h.end_block as u64) << shift,
                order.iter().enumerate().all(|(k, &i)| k == i),
                same_offsets
            );
            // byte-compare the padding: everything between entry blocks must be zero
            let mut f = File::open(&dat).unwrap_or_else(|x| die(x));
            let mut nonzero = 0u64;
            let mut buf = vec![0u8; 1 << 16];
            for (k, &i) in order.iter().enumerate() {
                let e = &idx.entries[i];
                let end = e.offset + e.raw_len();
                let next = order
                    .get(k + 1)
                    .map(|&j| idx.entries[j].offset)
                    .unwrap_or(total);
                let mut at = end;
                while at < next {
                    let n = ((next - at) as usize).min(buf.len());
                    f.seek(SeekFrom::Start(at))
                        .unwrap_or_else(|error| die(error));
                    f.read_exact(&mut buf[..n])
                        .unwrap_or_else(|error| die(error));
                    nonzero += buf[..n].iter().filter(|&&b| b != 0).count() as u64;
                    at += n as u64;
                }
            }
            println!("non-zero padding bytes between entries: {nonzero}");
        }
        "diff" => {
            let other = a.get(2).cloned().unwrap_or_else(|| die("missing <b.dat>"));
            let dict = load_dict(arg_value(a, "--dict").as_deref());
            let (mut r2, idx2) = open_index(context, &other);
            let ma: HashMap<u64, &qar::Entry> = idx.entries.iter().map(|e| (e.hash, e)).collect();
            let mb: HashMap<u64, &qar::Entry> = idx2.entries.iter().map(|e| (e.hash, e)).collect();
            let (mut same, mut changed, mut only_a, mut only_b, mut recoded) = (0, 0, 0, 0, 0);
            let mut keys: Vec<u64> = ma.keys().chain(mb.keys()).copied().collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                let (n, _) = name_of(k, &dict);
                match (ma.get(&k), mb.get(&k)) {
                    (Some(x), Some(y)) => {
                        let left_stored = stored(&mut r, x);
                        let right_stored = stored(&mut r2, y);
                        let left =
                            decoded(context, x, &left_stored).unwrap_or_else(|error| die(error));
                        let right =
                            decoded(context, y, &right_stored).unwrap_or_else(|error| die(error));
                        let identical_raw = left_stored == right_stored && x.md5 == y.md5;
                        if left == right {
                            same += 1;
                            if !identical_raw {
                                recoded += 1;
                            }
                        } else {
                            changed += 1;
                            println!("changed  {k:016x} {n}");
                        }
                    }
                    (Some(_), None) => {
                        only_a += 1;
                        println!("only A   {k:016x} {n}");
                    }
                    (None, Some(_)) => {
                        only_b += 1;
                        println!("only B   {k:016x} {n}");
                    }
                    _ => {}
                }
            }
            println!(
                "same content {same} (of which re-encoded {recoded}), changed {changed}, only in A {only_a}, only in B {only_b}"
            );
        }
        "rebuild" => {
            let out = a
                .get(2)
                .cloned()
                .unwrap_or_else(|| die("missing <out.dat>"));
            if h.extra_count != 0 || !idx.extra.is_empty() {
                die(
                    "rebuild does not yet support archives with extra records; input and output are unchanged",
                );
            }
            let input_path = std::fs::canonicalize(&dat).unwrap_or_else(|error| die(error));
            let output_path = crate::asset_paths::resolved_output(std::path::Path::new(&out))
                .unwrap_or_else(|error| die(error));
            if input_path == output_path {
                die("rebuild needs a different output path from its input archive");
            }
            let mut inputs = vec![input_path.as_path()];
            inputs.extend(profile_path);
            crate::asset_paths::protect_inputs(&[output_path], &inputs)
                .unwrap_or_else(|error| die(error));
            let keep = a.iter().any(|x| x == "--keep-padding");
            let total = std::fs::metadata(&dat).map(|m| m.len()).unwrap_or(0);
            let padding = padding_lengths(&idx, total).unwrap_or_else(|error| die(error));
            let mut list = Vec::with_capacity(idx.entries.len());
            let t = std::time::Instant::now();
            for e in &idx.entries {
                let mut raw = vec![0u8; e.raw_len() as usize];
                r.seek(SeekFrom::Start(e.offset)).unwrap_or_else(|x| die(x));
                r.read_exact(&mut raw).unwrap_or_else(|x| die(x));
                let pad = if keep {
                    let mut p = vec![0u8; padding[&e.offset]];
                    r.read_exact(&mut p).unwrap_or_else(|x| die(x));
                    Some(p)
                } else {
                    None
                };
                list.push(qar::RawEntry {
                    hash: e.hash,
                    raw,
                    pad,
                });
            }
            crate::output::write_atomic(std::path::Path::new(&out), |file| {
                let mut writer = std::io::BufWriter::with_capacity(1 << 22, file);
                context.write_archive(&mut writer, h.flags, h.version, &list)?;
                writer.flush()
            })
            .unwrap_or_else(|error| die(format!("{out}: {error}")));
            println!(
                "{} entries -> {out} in {:.1} s",
                list.len(),
                t.elapsed().as_secs_f64()
            );
        }
        _ => die("qar: info | list | verify | extract | relayout-check | diff | rebuild"),
    }
}

/// Index order is independent of the physical order of archive entry blocks.
fn padding_lengths(index: &qar::Index, archive_len: u64) -> Result<HashMap<u64, usize>, String> {
    let alignment = 1u64 << index.header.block_shift;
    let mut physical: Vec<_> = index.entries.iter().collect();
    physical.sort_by_key(|entry| entry.offset);
    let mut lengths = HashMap::new();
    for (position, entry) in physical.iter().enumerate() {
        let end = entry
            .offset
            .checked_add(entry.raw_len())
            .ok_or("entry extent overflow")?;
        let next = physical
            .get(position + 1)
            .map_or(archive_len, |entry| entry.offset);
        if next < end {
            return Err(format!("overlapping archive entry at {}", entry.offset));
        }
        let aligned =
            end.checked_add(alignment - 1).ok_or("alignment overflow")? / alignment * alignment;
        let padding = next.min(aligned) - end;
        let padding = usize::try_from(padding).map_err(|_| "padding exceeds platform limits")?;
        lengths.insert(entry.offset, padding);
    }
    Ok(lengths)
}
