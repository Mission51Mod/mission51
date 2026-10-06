//! Our own BC1 / BC3 (DXT1 / DXT5) encoder: the Phase-2 replacement for foxcore::dxt (the transitional, numpy-exact
//! port of flyk_look_assets.encode_dxt). Same interface and the same semantics (BC1 always 4-colour mode, no punch-
//! through; BC3 colour weighted by alpha > 8), but designed for quality and portability instead of reproducing
//! numpy's reduction orders:
//!   * plain sequential f64 arithmetic (IEEE basic operations and sqrt only: identical on every platform);
//!   * colour: principal axis (power iteration), extreme projections, then up to 4 least-squares refits, then a
//!     greedy +-1 search over the six quantised 565 channels of both endpoints (two passes), keeping the best;
//!   * single-colour blocks: the optimal endpoint pair per channel (precomputed tables) so palette entry 2 hits the
//!     colour as closely as 565 allows, instead of rounding to the nearest 565 colour;
//!   * alpha: 8-value mode from min / max, a least-squares refit and a +-2 endpoint search, and the 6-value mode
//!     (explicit 0 and 255) when it is better; error = squared difference.
//!
//! Evaluation against the transitional encoder: examples/dxt_eval.rs (PSNR / SSIM over corpus textures);
//! docs/formats/ftex.md "Phase 2". Decoding follows the BC1 / BC3 definitions (palette = exact thirds / sevenths /
//! fifths, rounded), the same model the error terms use.

use std::sync::OnceLock;

fn expand5(v: u16) -> f64 {
    ((v << 3) | (v >> 2)) as f64
}
fn expand6(v: u16) -> f64 {
    ((v << 2) | (v >> 4)) as f64
}

fn unpack565(c: u16) -> [f64; 3] {
    [expand5((c >> 11) & 31), expand6((c >> 5) & 63), expand5(c & 31)]
}

fn pack565(q: [i32; 3]) -> u16 {
    ((q[0] as u16) << 11) | ((q[1] as u16) << 5) | q[2] as u16
}

fn quant(c: [f64; 3]) -> [i32; 3] {
    let f = |v: f64, m: f64| (v.clamp(0.0, 255.0) * m / 255.0).round() as i32;
    [f(c[0], 31.0), f(c[1], 63.0), f(c[2], 31.0)]
}

/// BC1 4-colour palette of (c0, c1) as the decoder sees it
fn palette(c0: u16, c1: u16) -> [[f64; 3]; 4] {
    let (p0, p1) = (unpack565(c0), unpack565(c1));
    [p0, p1, std::array::from_fn(|k| ((2.0 * p0[k] + p1[k]) / 3.0).round()),
     std::array::from_fn(|k| ((p0[k] + 2.0 * p1[k]) / 3.0).round())]
}

/// indices + weighted squared error of a block for endpoints (c0 > c1 required for 4-colour mode)
fn fit(rgb: &[[f64; 3]; 16], w: &[f64; 16], c0: u16, c1: u16) -> ([u8; 16], f64) {
    fit_candidate::<false>(rgb, w, c0, c1, f64::INFINITY)
}

/// Partial indices are usable only when the returned error is below `limit`.
/// Pruning requires nonnegative weights, so later texels cannot lower the error.
fn fit_candidate<const PRUNE: bool>(rgb: &[[f64; 3]; 16], w: &[f64; 16], c0: u16, c1: u16, limit: f64) -> ([u8; 16], f64) {
    let pal = palette(c0, c1);
    let mut idx = [0u8; 16];
    let mut err = 0.0;
    for k in 0..16 {
        let mut best = (f64::INFINITY, 0u8);
        for (m, p) in pal.iter().enumerate() {
            let d = (rgb[k][0] - p[0]).powi(2) + (rgb[k][1] - p[1]).powi(2) + (rgb[k][2] - p[2]).powi(2);
            if d < best.0 {
                best = (d, m as u8);
            }
        }
        idx[k] = best.1;
        err += best.0 * w[k];
        if PRUNE && err >= limit {
            return (idx, err);
        }
    }
    (idx, err)
}

/// order a candidate pair for 4-colour mode; None when it collapses to one colour
fn order(a: [i32; 3], b: [i32; 3]) -> Option<(u16, u16)> {
    let (x, y) = (pack565(a), pack565(b));
    match x.cmp(&y) {
        std::cmp::Ordering::Greater => Some((x, y)),
        std::cmp::Ordering::Less => Some((y, x)),
        std::cmp::Ordering::Equal => None,
    }
}

/// per-channel optimal endpoint pairs for one colour value: palette entry 2 = (2 e0 + e1) / 3
struct SingleTables {
    five: [(u16, u16); 256],
    six: [(u16, u16); 256],
}

fn single_tables() -> &'static SingleTables {
    static T: OnceLock<SingleTables> = OnceLock::new();
    T.get_or_init(|| SingleTables { five: build_single_table(5), six: build_single_table(6) })
}

fn build_single_table(bits: u32) -> [(u16, u16); 256] {
    let max = (1u16 << bits) - 1;
    let ex = |v: u16| if bits == 5 { expand5(v) } else { expand6(v) };
    // Evaluate each endpoint pair once, retaining the first pair for each
    // reachable rounded palette value in the original a-major/b-minor order.
    let mut exact = [None; 256];
    for a in 0..=max {
        for b in 0..=max {
            let v = ((2.0 * ex(a) + ex(b)) / 3.0).round() as usize;
            if exact[v].is_none() {
                exact[v] = Some((a, b));
            }
        }
    }
    std::array::from_fn(|v| {
        if let Some(pair) = exact[v] {
            return pair;
        }
        let mut best = (usize::MAX, u32::MAX, (0u16, 0u16));
        for (value, &pair) in exact.iter().enumerate() {
            if let Some((a, b)) = pair {
                let error = value.abs_diff(v);
                let ordinal = a as u32 * (max as u32 + 1) + b as u32;
                // Equal errors retain the original search's earliest pair,
                // even when it produces the higher neighbouring value.
                if (error, ordinal) < (best.0, best.1) {
                    best = (error, ordinal, (a, b));
                }
            }
        }
        best.2
    })
}

fn bc1_block(rgb: &[[f64; 3]; 16], w: &[f64; 16]) -> [u8; 8] {
    let out = |c0: u16, c1: u16, idx: [u8; 16]| -> [u8; 8] {
        let mut o = [0u8; 8];
        o[0..2].copy_from_slice(&c0.to_le_bytes());
        o[2..4].copy_from_slice(&c1.to_le_bytes());
        let mut bits = 0u32;
        for (k, &i) in idx.iter().enumerate() {
            bits |= (i as u32) << (2 * k);
        }
        o[4..8].copy_from_slice(&bits.to_le_bytes());
        o
    };
    let wsum: f64 = w.iter().sum();
    let wsum = if wsum > 0.0 { wsum } else { 1.0 };
    let mut mean = [0.0f64; 3];
    for k in 0..16 {
        for c in 0..3 {
            mean[c] += rgb[k][c] * w[k];
        }
    }
    mean = mean.map(|m| m / wsum);
    // single colour (by weight): the optimal pair per channel, all indices 2
    let flat = (0..16).all(|k| w[k] < 0.5 || (0..3).all(|c| (rgb[k][c] - mean[c]).abs() < 0.5));
    let mut best: Option<(u16, u16, [u8; 16], f64)> = None;
    let consider = |c0: u16, c1: u16, best: &mut Option<(u16, u16, [u8; 16], f64)>| {
        debug_assert!(c0 > c1);
        let (idx, err) = fit(rgb, w, c0, c1);
        if best.as_ref().is_none_or(|b| err < b.3) {
            *best = Some((c0, c1, idx, err));
        }
        idx
    };
    if flat {
        let t = single_tables();
        let v = mean.map(|m| m.round().clamp(0.0, 255.0) as usize);
        let (r, g, b) = (t.five[v[0]], t.six[v[1]], t.five[v[2]]);
        let (c0, c1) = (pack565([r.0 as i32, g.0 as i32, b.0 as i32]), pack565([r.1 as i32, g.1 as i32, b.1 as i32]));
        if c0 > c1 {
            consider(c0, c1, &mut best);
        } else if c0 < c1 {
            // swapping the pair makes entry 3 the target colour
            consider(c1, c0, &mut best);
        }
    }
    // principal axis
    let mut cov = [[0.0f64; 3]; 3];
    for k in 0..16 {
        let d = [rgb[k][0] - mean[0], rgb[k][1] - mean[1], rgb[k][2] - mean[2]];
        for i in 0..3 {
            for j in 0..3 {
                cov[i][j] += d[i] * d[j] * w[k];
            }
        }
    }
    let mut axis = [1.0f64, 1.0, 1.0];
    for _ in 0..8 {
        let n = [0, 1, 2].map(|i| cov[i][0] * axis[0] + cov[i][1] * axis[1] + cov[i][2] * axis[2]);
        let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
        if len < 1e-12 {
            break;
        }
        axis = n.map(|x| x / len);
    }
    let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
    axis = axis.map(|x| x / len);
    let (mut tmin, mut tmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for k in 0..16 {
        if w[k] >= 0.5 || wsum < 0.5 {
            let t = (rgb[k][0] - mean[0]) * axis[0] + (rgb[k][1] - mean[1]) * axis[1] + (rgb[k][2] - mean[2]) * axis[2];
            tmin = tmin.min(t);
            tmax = tmax.max(t);
        }
    }
    if !tmin.is_finite() {
        tmin = 0.0;
        tmax = 0.0;
    }
    let mut e0 = [0, 1, 2].map(|c| mean[c] + axis[c] * tmax);
    let mut e1 = [0, 1, 2].map(|c| mean[c] + axis[c] * tmin);
    let sqrt_w = w.map(f64::sqrt);
    for _ in 0..4 {
        let (q0, q1) = (quant(e0), quant(e1));
        let Some((c0, c1)) = order(q0, q1) else {
            // collapsed: nudge one endpoint so 4-colour mode exists
            let mut q = q1;
            if q[1] > 0 { q[1] -= 1 } else { q[1] += 1 }
            if let Some((c0, c1)) = order(q0, q) {
                consider(c0, c1, &mut best);
            }
            break;
        };
        let idx = consider(c0, c1, &mut best);
        // least-squares refit of the (unquantised) endpoints from these indices
        const A: [f64; 4] = [1.0, 0.0, 2.0 / 3.0, 1.0 / 3.0];
        let (mut a11, mut a12, mut a22) = (0.0, 0.0, 0.0);
        let (mut b1, mut b2) = ([0.0f64; 3], [0.0f64; 3]);
        for k in 0..16 {
            let (a, b) = (A[idx[k] as usize] * sqrt_w[k], (1.0 - A[idx[k] as usize]) * sqrt_w[k]);
            a11 += a * a;
            a12 += a * b;
            a22 += b * b;
            for c in 0..3 {
                b1[c] += a * rgb[k][c] * sqrt_w[k];
                b2[c] += b * rgb[k][c] * sqrt_w[k];
            }
        }
        let det = a11 * a22 - a12 * a12;
        if det.abs() < 1e-9 {
            break;
        }
        e0 = [0, 1, 2].map(|c| (a22 * b1[c] - a12 * b2[c]) / det);
        e1 = [0, 1, 2].map(|c| (a11 * b2[c] - a12 * b1[c]) / det);
    }
    // greedy +-1 search on the quantised endpoints
    if let Some((c0, c1, _, _)) = best {
        // The public block API also accepts arbitrary weights; keep full scoring
        // when their errors can decrease. RGBA8 encoding uses 1 or 0.02.
        let can_prune = w.iter().all(|&v| v >= 0.0 && v.is_finite());
        let un = |c: u16| [((c >> 11) & 31) as i32, ((c >> 5) & 63) as i32, (c & 31) as i32];
        let mut q = [un(c0), un(c1)];
        for _ in 0..2 {
            if can_prune && best.as_ref().unwrap().3 == 0.0 {
                break;
            }
            let mut improved = false;
            for e in 0..2 {
                for ch in 0..3 {
                    let max = if ch == 1 { 63 } else { 31 };
                    for d in [-1, 1] {
                        let mut t = q;
                        t[e][ch] += d;
                        if t[e][ch] < 0 || t[e][ch] > max {
                            continue;
                        }
                        if let Some((a, b)) = order(t[0], t[1]) {
                            let before = best.as_ref().unwrap().3;
                            let (idx, err) = if can_prune {
                                fit_candidate::<true>(rgb, w, a, b, before)
                            } else {
                                fit(rgb, w, a, b)
                            };
                            if err < before {
                                best = Some((a, b, idx, err));
                                q = t;
                                improved = true;
                            }
                        }
                    }
                }
            }
            if !improved {
                break;
            }
        }
    }
    match best {
        Some((c0, c1, idx, _)) => out(c0, c1, idx),
        None => {
            // degenerate (all weights tiny and one colour): a 4-colour pair around the mean
            let q = quant(mean);
            let mut q1 = q;
            if q1[1] > 0 { q1[1] -= 1 } else { q1[1] += 1 }
            let (c0, c1) = order(q, q1).unwrap();
            let (idx, _) = fit(rgb, w, c0, c1);
            out(c0, c1, idx)
        }
    }
}

/// BC1 colour blocks (4-colour mode) for blocks of 16 texels
pub fn bc1_colour_blocks(rgb: &[[f64; 3]], wgt: &[f64]) -> Vec<[u8; 8]> {
    assert_eq!(rgb.len() % 16, 0);
    assert_eq!(rgb.len(), wgt.len());
    rgb.chunks_exact(16).zip(wgt.chunks_exact(16)).map(|(c, w)| bc1_block(c.try_into().unwrap(), w.try_into().unwrap())).collect()
}

fn alpha_palette(a0: i32, a1: i32) -> [f64; 8] {
    let (a, b) = (a0 as f64, a1 as f64);
    if a0 > a1 {
        std::array::from_fn(|i| match i {
            0 => a,
            1 => b,
            k => (((8 - k) as f64 * a + (k - 1) as f64 * b) / 7.0).round(),
        })
    } else {
        std::array::from_fn(|i| match i {
            0 => a,
            1 => b,
            6 => 0.0,
            7 => 255.0,
            k => (((6 - k) as f64 * a + (k - 1) as f64 * b) / 5.0).round(),
        })
    }
}

fn alpha_fit(alpha: &[f64], a0: i32, a1: i32, limit: f64) -> ([u8; 16], f64) {
    let pal = alpha_palette(a0, a1);
    let mut idx = [0u8; 16];
    let mut err = 0.0;
    for (k, &v) in alpha.iter().enumerate() {
        let mut best = (f64::INFINITY, 0u8);
        for (m, &p) in pal.iter().enumerate() {
            let d = (v - p) * (v - p);
            if d < best.0 {
                best = (d, m as u8);
            }
        }
        idx[k] = best.1;
        err += best.0;
        if err >= limit {
            return (idx, err);
        }
    }
    (idx, err)
}

fn bc3_alpha_block(alpha: &[f64; 16]) -> [u8; 8] {
    let a = alpha.map(|v| v.clamp(0.0, 255.0));
    let mx = a.iter().copied().fold(0.0, f64::max).round() as i32;
    let mn = a.iter().copied().fold(255.0, f64::min).round() as i32;
    let mut best: (f64, i32, i32, [u8; 16]) = (f64::INFINITY, 0, 0, [0; 16]);
    let try_pair = |a0: i32, a1: i32, best: &mut (f64, i32, i32, [u8; 16])| {
        if best.0 == 0.0 || !(0..=255).contains(&a0) || !(0..=255).contains(&a1) {
            return;
        }
        let (idx, err) = alpha_fit(&a, a0, a1, best.0);
        if err < best.0 {
            *best = (err, a0, a1, idx);
        }
    };
    if mx == mn {
        // flat: 8-value mode with the value as endpoint 0 (index 0 everywhere)
        if mx > 0 { try_pair(mx, mx - 1, &mut best) } else { try_pair(1, 0, &mut best) }
    } else {
        // 8-value mode around min / max
        for d0 in -2..=2 {
            for d1 in -2..=2 {
                if mx + d0 > mn + d1 {
                    try_pair(mx + d0, mn + d1, &mut best);
                }
            }
        }
        // least-squares refit of the 8-value endpoints from the best indices
        let (_, a0, a1, idx) = best;
        if a0 > a1 {
            let (mut s11, mut s12, mut s22, mut t1, mut t2) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for k in 0..16 {
                let f = match idx[k] { 0 => 1.0, 1 => 0.0, i => (8 - i as i32) as f64 / 7.0 };
                let g = 1.0 - f;
                s11 += f * f;
                s12 += f * g;
                s22 += g * g;
                t1 += f * a[k];
                t2 += g * a[k];
            }
            let det = s11 * s22 - s12 * s12;
            if det.abs() > 1e-9 {
                let n0 = ((s22 * t1 - s12 * t2) / det).round() as i32;
                let n1 = ((s11 * t2 - s12 * t1) / det).round() as i32;
                for d0 in -1..=1 {
                    for d1 in -1..=1 {
                        if n0 + d0 > n1 + d1 {
                            try_pair(n0 + d0, n1 + d1, &mut best);
                        }
                    }
                }
            }
        }
        // 6-value mode: endpoints span the values strictly between 0 and 255
        let (mut inner_count, mut lo, mut hi) = (0, 255.0f64, 0.0f64);
        for &v in &a {
            if v > 0.0 && v < 255.0 {
                inner_count += 1;
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        if inner_count > 0 && inner_count < 16 {
            let lo = lo.round() as i32;
            let hi = hi.round() as i32;
            for d0 in -1..=1 {
                for d1 in -1..=1 {
                    if lo + d0 <= hi + d1 {
                        try_pair(lo + d0, hi + d1, &mut best);
                    }
                }
            }
        }
    }
    let (_, a0, a1, idx) = best;
    let mut bits = 0u64;
    for (k, &i) in idx.iter().enumerate() {
        bits |= (i as u64) << (3 * k);
    }
    let mut o = [0u8; 8];
    o[0] = a0 as u8;
    o[1] = a1 as u8;
    o[2..8].copy_from_slice(&bits.to_le_bytes()[0..6]);
    o
}

/// BC3 alpha blocks for blocks of 16 texels
pub fn bc3_alpha_blocks(alpha: &[f64]) -> Vec<[u8; 8]> {
    assert_eq!(alpha.len() % 16, 0);
    alpha.chunks_exact(16).map(|a| bc3_alpha_block(a.try_into().unwrap())).collect()
}

/// RGBA8 pixels (w x h, row-major) -> BC1 (fmt 2) or BC3 (fmt 4) blocks; the interface of foxcore::dxt::encode_dxt
/// (edge padding to multiples of 4, at least 4 x 4; BC3 colour weights 1 where alpha > 8 else 0.02)
pub fn encode_dxt(rgba: &[u8], w: usize, h: usize, fmt: u16) -> Result<Vec<u8>, String> {
    if fmt != 2 && fmt != 4 {
        return Err(format!("encode_dxt: format {fmt} (2 = DXT1, 4 = DXT5)"));
    }
    if rgba.len() != w * h * 4 || w == 0 || h == 0 {
        return Err(format!("encode_dxt: {} bytes for {w} x {h} RGBA", rgba.len()));
    }
    let (bw, bh) = (4usize.max(w.div_ceil(4) * 4) / 4, 4usize.max(h.div_ceil(4) * 4) / 4);
    let mut out = Vec::with_capacity(bw * bh * if fmt == 2 { 8 } else { 16 });
    for by in 0..bh {
        for bx in 0..bw {
            let mut rgb = [[0.0f64; 3]; 16];
            let mut al = [0.0f64; 16];
            for j in 0..4 {
                let y = (by * 4 + j).min(h - 1);
                for i in 0..4 {
                    let x = (bx * 4 + i).min(w - 1);
                    let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                    rgb[4 * j + i] = [p[0] as f64, p[1] as f64, p[2] as f64];
                    al[4 * j + i] = p[3] as f64;
                }
            }
            let wgt: [f64; 16] = if fmt == 2 { [1.0; 16] } else { al.map(|a| if a > 8.0 { 1.0 } else { 0.02 }) };
            if fmt == 4 {
                out.extend_from_slice(&bc3_alpha_block(&al));
            }
            out.extend_from_slice(&bc1_block(&rgb, &wgt));
        }
    }
    Ok(out)
}

/// Decode BC1 / BC3 blocks to RGBA8 with the palette model the encoder optimises (exact thirds / sevenths / fifths,
/// rounded; BC1 c0 <= c1 = 3-colour + transparent black). For evaluation; the pipeline's Pillow-exact decoder is
/// foxpil::pil::bcn_decode.
pub fn decode(data: &[u8], w: usize, h: usize, fmt: u16) -> Vec<u8> {
    let bs = if fmt == 2 { 8 } else { 16 };
    let bw = w.div_ceil(4).max(1);
    let bh = h.div_ceil(4).max(1);
    let mut out = vec![0u8; w * h * 4];
    for by in 0..bh {
        for bx in 0..bw {
            let at = (by * bw + bx) * bs;
            let Some(blk) = data.get(at..at + bs) else { continue };
            let cb = &blk[bs - 8..];
            let c0 = u16::from_le_bytes([cb[0], cb[1]]);
            let c1 = u16::from_le_bytes([cb[2], cb[3]]);
            let (p0, p1) = (unpack565(c0), unpack565(c1));
            let pal: [[f64; 4]; 4] = if c0 > c1 || fmt == 4 {
                let p = palette(c0, c1);
                p.map(|c| [c[0], c[1], c[2], 255.0])
            } else {
                [[p0[0], p0[1], p0[2], 255.0], [p1[0], p1[1], p1[2], 255.0],
                 [((p0[0] + p1[0]) / 2.0).round(), ((p0[1] + p1[1]) / 2.0).round(), ((p0[2] + p1[2]) / 2.0).round(), 255.0],
                 [0.0, 0.0, 0.0, 0.0]]
            };
            let bits = u32::from_le_bytes(cb[4..8].try_into().unwrap());
            let (apal, abits) = if fmt == 4 {
                let mut b8 = [0u8; 8];
                b8[..6].copy_from_slice(&blk[2..8]);
                (Some(alpha_palette(blk[0] as i32, blk[1] as i32)), u64::from_le_bytes(b8))
            } else {
                (None, 0)
            };
            for k in 0..16 {
                let (x, y) = (bx * 4 + k % 4, by * 4 + k / 4);
                if x >= w || y >= h {
                    continue;
                }
                let c = pal[((bits >> (2 * k)) & 3) as usize];
                let o = (y * w + x) * 4;
                for ch in 0..3 {
                    out[o + ch] = c[ch] as u8;
                }
                out[o + 3] = match apal {
                    Some(ap) => ap[((abits >> (3 * k)) & 7) as usize] as u8,
                    None => c[3] as u8,
                };
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn single_tables_match_exhaustive_endpoint_oracle() {
        for bits in [5, 6] {
            let table = build_single_table(bits);
            let max = (1u16 << bits) - 1;
            let ex = |v| if bits == 5 { expand5(v) } else { expand6(v) };
            for (value, &pair) in table.iter().enumerate() {
                let mut expected = (f64::INFINITY, (0, 0));
                for a in 0..=max {
                    for b in 0..=max {
                        let error = (((2.0 * ex(a) + ex(b)) / 3.0).round() - value as f64).abs();
                        if error < expected.0 {
                            expected = (error, (a, b));
                        }
                    }
                }
                assert_eq!(pair, expected.1, "{bits}-bit table value {value}");
            }
        }
    }

    #[test]
    fn frozen_output_vectors() {
        // Original Phase-2 encoder bytes, recorded before optimization. Cover
        // alpha's zero/weight threshold/opaque values and mixed endpoint modes.
        for (alpha, expected) in [
            (0, [247, 3, 201, 99, 167, 204, 22, 33, 133, 212, 133, 31, 85, 255, 170, 0]),
            (8, [246, 9, 201, 99, 167, 204, 22, 33, 133, 212, 133, 31, 85, 255, 170, 0]),
            (9, [246, 9, 201, 99, 167, 204, 22, 33, 133, 212, 133, 31, 85, 255, 170, 0]),
            (255, [252, 8, 200, 97, 163, 196, 6, 1, 133, 212, 133, 31, 85, 255, 170, 0]),
        ] {
            let px: Vec<_> = (0..16).flat_map(|i| [i * 16, 255 - i * 8, 40, if i % 3 == 0 { alpha } else { i * 17 }]).collect();
            assert_eq!(encode_dxt(&px, 4, 4, 4).unwrap(), expected);
        }
    }

    #[test]
    fn colour_candidate_pruning_keeps_improvements() {
        let rgb = std::array::from_fn(|k| [k as f64 * 16.0, 255.0 - k as f64 * 8.0, 40.5]);
        let w = std::array::from_fn(|k| if k % 3 == 0 { 0.02 } else { 1.0 });
        for (c0, c1) in [(0xffff, 0), (0xaaaa, 0x1234), (0x80ff, 0x7fff)] {
            let (indices, error) = fit(&rgb, &w, c0, c1);
            for limit in [0.0, error / 2.0, error, error * 2.0, f64::INFINITY] {
                let (got_indices, got_error) = fit_candidate::<true>(&rgb, &w, c0, c1, limit);
                if got_error < limit {
                    assert_eq!(got_indices, indices);
                    assert_eq!(got_error.to_bits(), error.to_bits());
                } else {
                    assert!(error >= limit);
                }
            }
        }
    }

    #[test]
    fn flat_and_gradient_blocks() {
        // a flat colour that 565 rounding cannot hit: the single-colour tables get within 1 per channel
        let px = [123u8, 77, 201, 255].repeat(16);
        let b = encode_dxt(&px, 4, 4, 2).unwrap();
        let d = decode(&b, 4, 4, 2);
        for k in 0..16 {
            for c in 0..3 {
                assert!((d[4 * k + c] as i32 - px[4 * k + c] as i32).abs() <= 1, "{:?}", &d[..4]);
            }
        }
        // a gradient with alpha
        let px: Vec<u8> = (0..16).flat_map(|k| [k as u8 * 16, 255 - k as u8 * 8, 40, k as u8 * 17]).collect();
        let b = encode_dxt(&px, 4, 4, 4).unwrap();
        let d = decode(&b, 4, 4, 4);
        assert_eq!(d.len(), px.len());
        #[cfg(feature = "proof-compat")]
        {
            // a 240-step ramp in one block cannot be exact (4 palette entries); ours must not be worse than the reference
            let sse = |d: &[u8]| -> i64 { (0..64).map(|i| (d[i] as i64 - px[i] as i64).pow(2)).sum() };
            let r = decode(&crate::dxt::encode_dxt(&px, 4, 4, 4).unwrap(), 4, 4, 4);
            assert!(sse(&d) <= sse(&r), "ours {} > reference {}", sse(&d), sse(&r));
        }
        assert_eq!(encode_dxt(&px[..4], 1, 1, 4).unwrap().len(), 16);
    }
}
