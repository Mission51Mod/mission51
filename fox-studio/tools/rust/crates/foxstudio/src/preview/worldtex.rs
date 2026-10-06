//! World texture for the terrain previews: the location's tiles (`...X{ii}Z{jj}.ftex` or `.png`, tile i along x, tile
//! j along z, pixel row 0 = the smallest z: the FLYK bake's layout, checked against the heightfield) assembled
//! into one mosaic, row 0 = smallest z, covering the whole height grid.
use super::texture::{self, Image};
use std::path::{Path, PathBuf};

/// (i, j) from a tile file name ending in X{ii}Z{jj}.(ftex|png)
pub fn tile_index(name: &str) -> Option<(usize, usize)> {
    let lower = name.to_ascii_lowercase();
    let stem = lower
        .strip_suffix(".ftex")
        .or_else(|| lower.strip_suffix(".png"))?;
    let z = stem.rfind('z')?;
    let x = stem[..z].rfind('x')?;
    let i: usize = stem[x + 1..z].parse().ok()?;
    let j: usize = stem[z + 1..].parse().ok()?;
    Some((i, j))
}

/// the tiles under `dir` (recursive, bounded); .ftex preferred over .png for the same (i, j)
pub fn find_tiles(dir: &Path) -> Vec<(usize, usize, PathBuf)> {
    let files = crate::preview_view::list_files(&[dir.to_path_buf()], &["ftex", "png"], 4096);
    let mut out: Vec<(usize, usize, PathBuf)> = vec![];
    for f in files {
        let Some((i, j)) = f.file_name().and_then(|n| tile_index(&n.to_string_lossy())) else {
            continue;
        };
        let is_ftex = f
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("ftex"));
        match out.iter_mut().find(|t| t.0 == i && t.1 == j) {
            Some(t)
                if is_ftex
                    && !t
                        .2
                        .extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("ftex")) =>
            {
                t.2 = f
            }
            Some(_) => {}
            None => out.push((i, j, f)),
        }
    }
    out.sort();
    out
}

/// a tile scaled to `side` x `side` (an ftex mip of that size when there is one, else a box filter)
fn tile_image(path: &Path, side: usize) -> Result<Image, String> {
    let t = texture::load(path, 16)?;
    let lvl = t
        .levels
        .iter()
        .find(|l| l.width <= side.max(1))
        .unwrap_or_else(|| t.levels.last().unwrap());
    Ok(resize(lvl, side))
}

/// box / nearest resize to side x side
pub fn resize(img: &Image, side: usize) -> Image {
    if img.width == side && img.height == side {
        return img.clone();
    }
    let mut out = vec![0u8; side * side * 4];
    let (fx, fy) = (
        img.width as f32 / side as f32,
        img.height as f32 / side as f32,
    );
    for y in 0..side {
        for x in 0..side {
            let (x0, y0) = ((x as f32 * fx) as usize, (y as f32 * fy) as usize);
            let (x1, y1) = (
                ((x + 1) as f32 * fx).ceil() as usize,
                ((y + 1) as f32 * fy).ceil() as usize,
            );
            let (x1, y1) = (x1.clamp(x0 + 1, img.width), y1.clamp(y0 + 1, img.height));
            let mut acc = [0u32; 4];
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = 4 * (yy * img.width + xx);
                    for (channel, total) in acc.iter_mut().enumerate() {
                        *total += img.rgba[i + channel] as u32;
                    }
                }
            }
            let n = ((x1 - x0) * (y1 - y0)) as u32;
            let o = 4 * (y * side + x);
            for c in 0..4 {
                out[o + c] = (acc[c] / n) as u8;
            }
        }
    }
    Image {
        width: side,
        height: side,
        rgba: out,
    }
}

/// Assemble the tiles into one image (row 0 = smallest z), `tile_px` per tile. Missing tiles stay grey.
pub fn mosaic(
    tiles: &[(usize, usize, PathBuf)],
    tile_px: usize,
) -> Result<(Image, Vec<String>), String> {
    if tiles.is_empty() {
        return Err("no world-texture tiles (…X00Z00.ftex / .png)".into());
    }
    let n = tiles.iter().map(|t| t.0.max(t.1) + 1).max().unwrap_or(1);
    let side = n * tile_px;
    let mut rgba = vec![128u8; side * side * 4];
    let mut notes = vec![];
    for (i, j, p) in tiles {
        match tile_image(p, tile_px) {
            Ok(img) => {
                for y in 0..tile_px {
                    let dst = 4 * ((j * tile_px + y) * side + i * tile_px);
                    rgba[dst..dst + 4 * tile_px]
                        .copy_from_slice(&img.rgba[4 * y * tile_px..4 * (y + 1) * tile_px]);
                }
            }
            Err(e) => notes.push(format!("{}: {e}", p.display())),
        }
    }
    for px in rgba.chunks_exact_mut(4) {
        px[3] = 255;
    }
    Ok((
        Image {
            width: side,
            height: side,
            rgba,
        },
        notes,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_names() {
        assert_eq!(tile_index("cm_flyk_wrtx001X02Z03.ftex"), Some((2, 3)));
        assert_eq!(tile_index("flyk_wrtx_X00Z01.png"), Some((0, 1)));
        assert_eq!(tile_index("flyk_wrtx_X00Z01.1.ftexs"), None);
        assert_eq!(tile_index("other.png"), None);
    }

    #[test]
    fn mosaic_places_tiles_by_index() {
        let d = std::env::temp_dir().join(format!("foxstudio_wt_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        for (i, j, c) in [
            (0, 0, [255u8, 0, 0, 255]),
            (1, 0, [0, 255, 0, 255]),
            (0, 1, [0, 0, 255, 255]),
            (1, 1, [255, 255, 255, 255]),
        ] {
            let px: Vec<u8> = std::iter::repeat_n(c, 8 * 8).flatten().collect();
            image::save_buffer(
                d.join(format!("t_X{i:02}Z{j:02}.png")),
                &px,
                8,
                8,
                image::ExtendedColorType::Rgba8,
            )
            .unwrap();
        }
        let tiles = find_tiles(&d);
        assert_eq!(tiles.len(), 4);
        let (m, notes) = mosaic(&tiles, 4).unwrap();
        assert!(notes.is_empty());
        assert_eq!((m.width, m.height), (8, 8));
        let at = |x: usize, y: usize| m.rgba[4 * (y * 8 + x)..4 * (y * 8 + x) + 3].to_vec();
        assert_eq!(at(0, 0), vec![255, 0, 0]); // i 0, j 0: x low, z low (row 0)
        assert_eq!(at(7, 0), vec![0, 255, 0]); // i 1: x high
        assert_eq!(at(0, 7), vec![0, 0, 255]); // j 1: z high
        let _ = std::fs::remove_dir_all(&d);
    }
}
