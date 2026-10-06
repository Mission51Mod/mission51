//! One dispatch point for the codec-porter formats (M2): decode + re-encode a file by its kind.
//! Used by the vanilla-corpus runner (examples/codec_corpus.rs) and by `fox roundtrip` (tooling).

/// kinds with a reader + writer in foxcore (the file extension, lowercase, no dot), plus the read-only Wwise kinds
/// ("wem", and "bnk" = the SoundBanks inside .sbp files), whose "writer" reassembles the parsed chunks.
pub const KINDS: &[&str] = &["bnk", "des", "fdes", "fmdl", "fox2", "frdv", "frt", "fsm", "fstb", "ftex", "geom", "geoms", "grxla", "gskl", "lba", "lng2", "lpsh", "obr", "obrb", "sand", "subp",
                             "twpf", "vfxlb", "vfxlf", "wem"];

/// the file extension a kind is found in (bnk lives inside .sbp)
pub fn source_ext(kind: &str) -> &str {
    match kind {
        "bnk" => "sbp",
        k => k.strip_suffix("+xml").unwrap_or(k),
    }
}

/// extra checks: binary fox2 -> datfpk-dialect XML -> binary (fox2::decompile / compile) must give the input back
pub const XML_KINDS: &[&str] = &["des+xml", "fox2+xml", "vfxlf+xml"];

/// decode + re-encode `b` (a file of `source_ext(kind)`) as `kind`; None when foxcore has no codec for it.
/// "ftex" round-trips the header only; the .ftex + .ftexs set check is ftex::roundtrip_set.
pub fn roundtrip(kind: &str, b: &[u8]) -> Option<Result<Vec<u8>, String>> {
    Some(match kind {
        "bnk" => sbp_banks(b),
        "des+xml" | "fox2+xml" | "vfxlf+xml" => crate::fox2::decompile(b).and_then(|x| crate::fox2::compile(&x)),
        // binary fox2 (magic F2 "box") under other extensions: the foxcore::fox2 codec (tooling)
        "des" | "fox2" | "vfxlf" => crate::fox2::read(b).and_then(|f| crate::fox2::write(&f)),
        "fdes" => crate::fdes::parse(b).and_then(|m| crate::fdes::write(&m)),
        "fmdl" => crate::fmdl::read(b).map(|f| f.build()),
        "frdv" => crate::frdv::read(b).map(|f| crate::frdv::write(&f)),
        "frt" => crate::frt::parse(b).and_then(|r| crate::frt::build(&r)),
        "fsm" => crate::fsm::read(b).and_then(|f| {
            // TrackStream packets are type 0; a few DEMO chunks carry other packet types (kept as bytes)
            for c in f.chunks.iter().filter(|c| &c.tag == b"DEMO" && c.payload.get(0..4) == Some(&[0, 0, 0, 0])) {
                crate::fsm::packet(&c.payload)?;
            }
            Ok(crate::fsm::write(&f))
        }),
        "fstb" => crate::fstb::read(b).and_then(|f| crate::fstb::write(&f)),
        "ftex" => crate::ftex::read(b).map(|f| crate::ftex::write(&f)),
        "geom" | "geoms" | "gskl" => crate::foxdata::read(b).map(|f| crate::foxdata::write(&f)),
        "grxla" => crate::grxla::read(b).map(|g| crate::grxla::write(&g)),
        "lba" => crate::lba::read(b).and_then(|l| crate::lba::write(&l)),
        "lng2" => crate::lng2::read(b).map(|l| crate::lng2::write(&l)),
        "lpsh" => crate::lpsh::read(b).and_then(|l| crate::lpsh::write(&l)),
        "obr" | "obrb" => crate::obr::read(b).map(|o| crate::obr::write(&o)),
        "sand" => crate::sand::read(b).and_then(|s| crate::sand::write(&s)),
        "subp" => crate::subp::read(b).and_then(|s| crate::subp::write(&s)),
        "twpf" => crate::twpf::parse(b).map(|t| crate::twpf::write(&t)),
        "vfxlb" => crate::vfxlb::parse(b).and_then(|v| crate::vfxlb::write(&v)),
        "wem" => crate::wwise::parse_wem(b).and_then(|_| crate::wwise::reassemble_riff(b)),
        _ => return None,
    })
}

/// every "bnk" blob of an .sbp: sections reassembled, HIRC objects parsed; the .sbp rebuilt from the results
fn sbp_banks(b: &[u8]) -> Result<Vec<u8>, String> {
    let mut s = crate::containers::sbp_read(b)?;
    for (tag, blob) in s.entries.iter_mut() {
        if tag == b"bnk\0" {
            let sec = crate::wwise::bank_sections(blob)?;
            for c in &sec {
                if &c.id == b"HIRC" {
                    crate::wwise::read_hirc(&blob[c.offset..c.offset + c.size])?;
                }
            }
            *blob = crate::wwise::reassemble_bank(blob)?;
        }
    }
    crate::containers::sbp_write(&s)
}
