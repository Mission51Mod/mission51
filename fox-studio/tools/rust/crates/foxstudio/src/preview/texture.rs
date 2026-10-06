//! Texture previews: .ftex (+ its .N.ftexs streams), .dds and .png decoded to RGBA8 on a worker. Read only.
//!
//! - The .ftex container comes from `foxcore::ftex` (the codec porter's reader; docs/formats/ftex.md). It serves
//!   stream-0 mips from the .ftex itself, and 3-D textures come out at w * h * depth.
//! - BC1 / BC3 decoding uses our checked foxcore::block_texture readers; no Pillow identity is claimed.
//! - BGRA8 (format 0) and the 8-bit single channel (format 1) are the two small helpers here.
//!
//! Vanilla census (24,064 loose .ftex): 2 = DXT1 14,555; 4 = DXT5 9,479; 0 = BGRA8 24 (incl. 3-D LUTs);
//! 1 = 8-bit 6. Nothing else occurs.
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Bgra8,
    Rgba8,
    L8,
    Bc1,
    Bc3,
}

impl PixelFormat {
    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Bgra8 => "BGRA8",
            PixelFormat::Rgba8 => "RGBA8",
            PixelFormat::L8 => "8-bit",
            PixelFormat::Bc1 => "BC1 (DXT1)",
            PixelFormat::Bc3 => "BC3 (DXT5)",
        }
    }
    /// bytes for a w x h image
    pub fn size(self, w: usize, h: usize) -> usize {
        let blocks = w.div_ceil(4) * h.div_ceil(4);
        match self {
            PixelFormat::Bgra8 | PixelFormat::Rgba8 => w * h * 4,
            PixelFormat::L8 => w * h,
            PixelFormat::Bc1 => blocks * 8,
            PixelFormat::Bc3 => blocks * 16,
        }
    }
    /// the .ftex pixel-format code
    pub fn from_ftex(code: u16) -> Option<PixelFormat> {
        match code {
            0 => Some(PixelFormat::Bgra8),
            1 => Some(PixelFormat::L8),
            2 => Some(PixelFormat::Bc1),
            4 => Some(PixelFormat::Bc3),
            _ => None,
        }
    }
}

/// a decoded image level
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    /// RGBA8, row-major
    pub rgba: Vec<u8>,
}

/// A loaded texture: every level decoded (level 0 first), and what it is.
#[derive(Clone, Debug)]
pub struct Texture {
    pub path: PathBuf,
    pub kind: &'static str,
    pub format: String,
    pub width: usize,
    pub height: usize,
    pub depth: usize,
    pub levels: Vec<Image>,
    /// what could not be shown (missing stream file, 3-D slices, ...)
    pub notes: Vec<String>,
}

/// <stem>.<n>.ftexs next to the .ftex
pub fn stream_path(ftex: &Path, stream: u8) -> PathBuf {
    let s = ftex.to_string_lossy();
    let stem = s.strip_suffix(".ftex").unwrap_or(&s);
    PathBuf::from(format!("{stem}.{stream}.ftexs"))
}

/// The stream file: next to the .ftex, else the same Assets path in the other unpacked archive folders (patch .ftex
/// in master/0/01 keep their .ftexs in texture0, ...): the archive folder's siblings and its parent's siblings.
pub fn find_stream(ftex: &Path, stream: u8) -> Option<PathBuf> {
    let p = stream_path(ftex, stream);
    if p.is_file() {
        return Some(p);
    }
    let comps: Vec<_> = p.components().collect();
    let k = comps.iter().rposition(|c| c.as_os_str().eq_ignore_ascii_case("Assets"))?;
    let rel: PathBuf = comps[k..].iter().collect();
    let archive: PathBuf = comps[..k].iter().collect();
    let mut roots = vec![];
    if let Some(parent) = archive.parent() {
        roots.push(parent.to_path_buf());
        if let Some(gp) = parent.parent() {
            roots.push(gp.to_path_buf());
        }
    }
    for r in roots {
        let Ok(rd) = std::fs::read_dir(&r) else { continue };
        let mut dirs: Vec<PathBuf> = rd.flatten().filter(|e| e.file_type().is_ok_and(|t| t.is_dir())).map(|e| e.path()).collect();
        dirs.sort();
        for d in dirs {
            let c = d.join(&rel);
            if c.is_file() {
                return Some(c);
            }
        }
    }
    None
}

/// Load any supported texture file (.ftex, .dds, .png). `max_levels` limits decoding work for huge mip chains.
pub fn load(path: &Path, max_levels: usize) -> Result<Texture, String> {
    let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match ext.as_str() {
        "ftex" => load_ftex(path, &bytes, max_levels),
        "dds" => load_dds(path, &bytes, max_levels),
        "png" => {
            let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?.to_rgba8();
            let (w, h) = (img.width() as usize, img.height() as usize);
            Ok(Texture { path: path.to_path_buf(), kind: "PNG", format: "RGBA8".into(), width: w, height: h, depth: 1,
                         levels: vec![Image { width: w, height: h, rgba: img.into_raw() }], notes: vec![] })
        }
        _ => Err(format!("{}: not a texture this preview reads (.ftex, .dds, .png)", path.display())),
    }
}

fn load_ftex(path: &Path, b: &[u8], max_levels: usize) -> Result<Texture, String> {
    let mut missing: Vec<String> = vec![];
    let (f, mips) = foxcore::ftex::ftex_mips(b, &mut |n| match find_stream(path, n) {
        Some(p) => std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display())),
        None => {
            missing.push(stream_path(path, n).file_name().unwrap_or_default().to_string_lossy().into_owned());
            Err(format!("{} missing", stream_path(path, n).display()))
        }
    })?;
    let code = f.format();
    let fmt = PixelFormat::from_ftex(code).ok_or_else(|| format!("ftex pixel format {code} is not one the vanilla files use"))?;
    let (w0, h0, depth) = (f.width() as usize, f.height() as usize, (f.depth() as usize).max(1));
    let mut levels = vec![];
    let mut notes = vec![];
    for (&mip, data) in mips.iter().take(max_levels.max(1)) {
        let (w, h) = ((w0 >> mip).max(1), (h0 >> mip).max(1));
        // 3-D textures: the first slice
        match decode(fmt, data, w, h) {
            Ok(img) => levels.push(img),
            Err(e) => notes.push(format!("mip {mip}: {e}")),
        }
    }
    if levels.is_empty() {
        return Err(format!("no level decoded: {}", notes.join("; ")));
    }
    if depth > 1 {
        notes.push(format!("3-D texture: {depth} slices, showing the first"));
    }
    Ok(Texture { path: path.to_path_buf(), kind: "FTEX", format: format!("{} (ftex format {code})", fmt.name()), width: w0,
                 height: h0, depth, levels, notes })
}

fn u32le(b: &[u8], o: usize) -> Result<u32, String> {
    b.get(o..o + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| format!("truncated at {o}"))
}

fn load_dds(path: &Path, b: &[u8], max_levels: usize) -> Result<Texture, String> {
    if b.get(0..4) != Some(b"DDS ") || b.len() < 128 {
        return Err("not a .dds (no DDS magic)".into());
    }
    let h = u32le(b, 12)? as usize;
    let w = u32le(b, 16)? as usize;
    let mips = (u32le(b, 28)? as usize).max(1);
    let pf_flags = u32le(b, 80)?;
    let four = b.get(84..88).ok_or("truncated")?;
    let bits = u32le(b, 88)?;
    let rmask = u32le(b, 92)?;
    let mut data_off = 128;
    let fmt = if pf_flags & 0x4 != 0 {
        match four {
            b"DXT1" => PixelFormat::Bc1,
            b"DXT4" | b"DXT5" => PixelFormat::Bc3,
            b"DX10" => {
                data_off = 148;
                match u32le(b, 128)? {
                    70..=72 => PixelFormat::Bc1,
                    76..=78 => PixelFormat::Bc3,
                    27..=29 => PixelFormat::Rgba8,
                    87 | 90 | 91 => PixelFormat::Bgra8,
                    d => return Err(format!("DXGI format {d} is not supported by the preview")),
                }
            }
            f => return Err(format!("DDS FourCC {:?} is not supported by the preview", String::from_utf8_lossy(f))),
        }
    } else if bits == 32 {
        if rmask == 0x0000_00FF { PixelFormat::Rgba8 } else { PixelFormat::Bgra8 }
    } else if bits == 8 {
        PixelFormat::L8
    } else {
        return Err(format!("uncompressed {bits}-bit DDS is not supported by the preview"));
    };
    let mut levels = vec![];
    let mut off = data_off;
    for l in 0..mips.min(max_levels.max(1)) {
        let (lw, lh) = ((w >> l).max(1), (h >> l).max(1));
        let n = fmt.size(lw, lh);
        let Some(d) = b.get(off..off + n) else { break };
        levels.push(decode(fmt, d, lw, lh)?);
        off += n;
    }
    if levels.is_empty() {
        return Err("no level decoded (truncated file)".into());
    }
    Ok(Texture { path: path.to_path_buf(), kind: "DDS", format: fmt.name().into(), width: w, height: h, depth: 1, levels, notes: vec![] })
}

/// Decode one level to RGBA8 (the first w x h slice of `d`).
pub fn decode(fmt: PixelFormat, d: &[u8], w: usize, h: usize) -> Result<Image, String> {
    let need = fmt.size(w, h);
    if d.len() < need {
        return Err(format!("{} bytes, {} x {} {} needs {need}", d.len(), w, h, fmt.name()));
    }
    let rgba = match fmt {
        PixelFormat::Bc1 => foxcore::block_texture::decode_bc1(&d[..need], w, h)?,
        PixelFormat::Bc3 => foxcore::block_texture::decode_bc3(&d[..need], w, h)?,
        PixelFormat::Bgra8 => d[..need].chunks_exact(4).flat_map(|s| [s[2], s[1], s[0], s[3]]).collect(),
        PixelFormat::Rgba8 => d[..need].to_vec(),
        PixelFormat::L8 => d[..need].iter().flat_map(|&v| [v, v, v, 255]).collect(),
    };
    Ok(Image { width: w, height: h, rgba })
}

/// Which channels to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Channels {
    #[default]
    Rgba,
    Rgb,
    R,
    G,
    B,
    A,
}

/// apply a channel view (alpha shown on a checkerboard for Rgba; single channels as grey)
pub fn view(img: &Image, ch: Channels) -> Vec<u8> {
    let mut out = img.rgba.clone();
    for (i, px) in out.chunks_exact_mut(4).enumerate() {
        let (x, y) = (i % img.width, i / img.width);
        match ch {
            Channels::Rgba => {
                let bg = if ((x / 8) + (y / 8)) % 2 == 0 { 200u32 } else { 150 };
                let a = px[3] as u32;
                for channel in &mut px[..3] {
                    *channel = ((*channel as u32 * a + bg * (255 - a)) / 255) as u8;
                }
                px[3] = 255;
            }
            Channels::Rgb => px[3] = 255,
            Channels::R | Channels::G | Channels::B | Channels::A => {
                let v = px[match ch {
                    Channels::R => 0,
                    Channels::G => 1,
                    Channels::B => 2,
                    _ => 3,
                }];
                px.copy_from_slice(&[v, v, v, 255]);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bc1_endpoints_and_partial_blocks() {
        // c0 = pure red (0xF800) > c1 = pure blue (0x001F): 4-colour mode; first row indices 0, 1
        let blk = [0x00, 0xF8, 0x1F, 0x00, 0b0000_0100, 0, 0, 0];
        let img = decode(PixelFormat::Bc1, &blk, 4, 4).unwrap();
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]);
        assert_eq!(&img.rgba[4..8], &[0, 0, 255, 255]);
        // a 3 x 2 image still takes one block
        let img = decode(PixelFormat::Bc1, &blk, 3, 2).unwrap();
        assert_eq!(img.rgba.len(), 3 * 2 * 4);
        assert!(decode(PixelFormat::Bc3, &blk, 4, 4).is_err());
    }

    #[test]
    fn bgra_l8_and_views() {
        let img = decode(PixelFormat::Bgra8, &[1, 2, 3, 4], 1, 1).unwrap();
        assert_eq!(img.rgba, vec![3, 2, 1, 4]);
        assert_eq!(view(&img, Channels::R), vec![3, 3, 3, 255]);
        assert_eq!(view(&img, Channels::A), vec![4, 4, 4, 255]);
        assert_eq!(view(&img, Channels::Rgb), vec![3, 2, 1, 255]);
        assert_eq!(decode(PixelFormat::L8, &[9], 1, 1).unwrap().rgba, vec![9, 9, 9, 255]);
        assert_eq!(PixelFormat::from_ftex(3), None);
    }

    /// a synthetic .ftex whose stream lives in another archive folder (patch .ftex in 0_01, .ftexs in texture0)
    #[test]
    fn synthetic_ftex_with_stream_elsewhere() {
        use std::io::Write;
        let d = std::env::temp_dir().join(format!("foxstudio_ftex_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let rel = "Assets/tpp/x/t";
        std::fs::create_dir_all(d.join("0_01").join(rel).parent().unwrap()).unwrap();
        std::fs::create_dir_all(d.join("texture0").join(rel).parent().unwrap()).unwrap();
        // 8 x 8 BGRA = 256 bytes: two chunks of 128 (one zlib, one raw)
        let pixels: Vec<u8> = (0..256u32).map(|i| (i * 7 % 251) as u8).collect();
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&pixels[..128]).unwrap();
        let z = z.finish().unwrap();
        let mut mip = vec![];
        let body0 = 16u32;
        mip.extend((z.len() as u16).to_le_bytes());
        mip.extend(128u16.to_le_bytes());
        mip.extend(body0.to_le_bytes());
        mip.extend(128u16.to_le_bytes());
        mip.extend(128u16.to_le_bytes());
        mip.extend((0x8000_0000u32 | (body0 + z.len() as u32)).to_le_bytes());
        mip.extend(&z);
        mip.extend(&pixels[128..]);
        let mut stream = vec![0u8; 32];
        stream.extend(&mip);
        let mut head = vec![0u8; 0x50];
        head[0..4].copy_from_slice(b"FTEX");
        head[4..8].copy_from_slice(&2.03f32.to_bits().to_le_bytes());
        head[10..12].copy_from_slice(&8u16.to_le_bytes());
        head[12..14].copy_from_slice(&8u16.to_le_bytes());
        head[14..16].copy_from_slice(&1u16.to_le_bytes());
        head[0x10] = 1;
        head[0x40..0x44].copy_from_slice(&32u32.to_le_bytes());
        head[0x44..0x48].copy_from_slice(&256u32.to_le_bytes());
        head[0x48..0x4C].copy_from_slice(&(mip.len() as u32).to_le_bytes());
        head[0x4D] = 1;
        head[0x4E..0x50].copy_from_slice(&2u16.to_le_bytes());
        let f = d.join("0_01").join(format!("{rel}.ftex"));
        std::fs::write(&f, &head).unwrap();
        // no stream anywhere: a clear error
        assert!(load(&f, 8).unwrap_err().contains("missing"));
        std::fs::write(d.join("texture0").join(format!("{rel}.1.ftexs")), &stream).unwrap();
        assert_eq!(find_stream(&f, 1), Some(d.join("texture0").join(format!("{rel}.1.ftexs"))));
        let t = load(&f, 8).unwrap();
        assert_eq!((t.width, t.height, t.levels.len()), (8, 8, 1));
        assert_eq!(t.levels[0].rgba[0..4], [pixels[2], pixels[1], pixels[0], pixels[3]]);
        assert_eq!(t.levels[0].rgba[252..256], [pixels[254], pixels[253], pixels[252], pixels[255]]);
        let _ = std::fs::remove_dir_all(&d);
    }
}
