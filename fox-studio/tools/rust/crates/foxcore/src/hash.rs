//! Fox Engine name hashes: StrCode64 / StrCode32 / PathCode64, on CityHash64 (v1.0.3 behaviour).
//!
//! Our own implementation, written from the algorithm's description: the four 64-bit constants and the
//! 128 -> 64-bit mixing multiplier that define the hash, and its length classes (0-16, 17-32, 33-64 bytes, and the
//! 64-byte block loop over longer inputs, with a 32-byte "weak" two-lane mix). No existing source was consulted.
//! It replaced a transcription of the reference code on 2026-10-05 after a proof of identity (the old and new
//! implementations side by side, examples/hash_proof.rs in the porter's scratch workspace): 3,462,207 distinct names
//! of the community dictionaries (StrCode64 / 32, PathCode64 with and without an extension id, QAR file hash),
//! 39,451 dictionary-named QAR entries of 10 test / sandbox archives (v2 hash = stored hash), and 161,600 synthetic
//! inputs of length 0-4095 (city64 and city64_with_seeds): all identical (work/rust_port/codecs/PROGRESS.txt).
//!
//! StrCode64(text)  = CityHash64WithSeeds(text + NUL, K2, (text[0] << 16) + len(text)) & (2^48 - 1)
//!                    (seed 0 for the empty string)
//! StrCode32(text)  = low 32 bits of StrCode64
//! PathCode64(path) = GzsTool's HashFileName: the extension (everything after the first '.') dropped, "/Assets/"
//!                    stripped (else bit 50 set), CityHash64WithSeeds(text, K2, the last 8 bytes reversed as an
//!                    integer) & (2^50 - 1), then the extension id in bits 51+.

const K0: u64 = 0xc3a5_c85c_97cb_3127;
const K1: u64 = 0xb492_b66f_be98_f273;
pub const K2: u64 = 0x9ae1_6a3b_2f90_404f;
const K3: u64 = 0xc949_d7c7_509e_6557;
const MIX: u64 = 0x9ddf_ea08_eb38_2d69;

#[inline]
fn word(s: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(s[at..at + 8].try_into().unwrap())
}

#[inline]
fn half(s: &[u8], at: usize) -> u64 {
    u32::from_le_bytes(s[at..at + 4].try_into().unwrap()) as u64
}

/// fold the top 17 bits back in
#[inline]
fn fold(v: u64) -> u64 {
    v ^ (v >> 47)
}

/// 128 -> 64-bit reduction of the pair (lo, hi)
#[inline]
fn pair(lo: u64, hi: u64) -> u64 {
    let a = fold((lo ^ hi).wrapping_mul(MIX));
    fold((hi ^ a).wrapping_mul(MIX)).wrapping_mul(MIX)
}

#[inline]
fn ror(v: u64, n: u32) -> u64 {
    v.rotate_right(n)
}

fn short(s: &[u8]) -> u64 {
    let n = s.len();
    match n {
        0 => K2,
        1..=3 => {
            let y = (s[0] as u32).wrapping_add((s[n / 2] as u32) << 8) as u64;
            let z = (n as u32).wrapping_add((s[n - 1] as u32) << 2) as u64;
            fold(y.wrapping_mul(K2) ^ z.wrapping_mul(K3)).wrapping_mul(K2)
        }
        4..=8 => pair((n as u64).wrapping_add(half(s, 0) << 3), half(s, n - 4)),
        _ => {
            let (a, b) = (word(s, 0), word(s, n - 8));
            // n is 9..16 here, so the rotation is by 9..16 bits
            pair(a, ror(b.wrapping_add(n as u64), n as u32)) ^ b
        }
    }
}

fn medium(s: &[u8]) -> u64 {
    let n = s.len();
    let a = word(s, 0).wrapping_mul(K1);
    let b = word(s, 8);
    let c = word(s, n - 8).wrapping_mul(K2);
    let d = word(s, n - 16).wrapping_mul(K0);
    pair(ror(a.wrapping_sub(b), 43).wrapping_add(ror(c, 30)).wrapping_add(d),
         a.wrapping_add(ror(b ^ K3, 20)).wrapping_sub(c).wrapping_add(n as u64))
}

/// one 32-byte half: four words mixed into two lanes from the seeds (a, b)
fn lanes(s: &[u8], at: usize, a: u64, b: u64) -> (u64, u64) {
    let (w, x, y, z) = (word(s, at), word(s, at + 8), word(s, at + 16), word(s, at + 24));
    let mut a = a.wrapping_add(w);
    let mut b = ror(b.wrapping_add(a).wrapping_add(z), 21);
    let c = a;
    a = a.wrapping_add(x).wrapping_add(y);
    b = b.wrapping_add(ror(a, 44));
    (a.wrapping_add(z), b.wrapping_add(c))
}

fn upto64(s: &[u8]) -> u64 {
    let n = s.len();
    let nn = n as u64;
    // first half: from the front
    let z = word(s, 24);
    let mut a = word(s, 0).wrapping_add(nn.wrapping_add(word(s, n - 16)).wrapping_mul(K0));
    let mut b = ror(a.wrapping_add(z), 52);
    let mut c = ror(a, 37);
    a = a.wrapping_add(word(s, 8));
    c = c.wrapping_add(ror(a, 7));
    a = a.wrapping_add(word(s, 16));
    let vf = a.wrapping_add(z);
    let vs = b.wrapping_add(ror(a, 31)).wrapping_add(c);
    // second half: from the back
    a = word(s, 16).wrapping_add(word(s, n - 32));
    let z = word(s, n - 8);
    b = ror(a.wrapping_add(z), 52);
    c = ror(a, 37);
    a = a.wrapping_add(word(s, n - 24));
    c = c.wrapping_add(ror(a, 7));
    a = a.wrapping_add(word(s, n - 16));
    let wf = a.wrapping_add(z);
    let ws = b.wrapping_add(ror(a, 31)).wrapping_add(c);
    let r = fold(vf.wrapping_add(ws).wrapping_mul(K2).wrapping_add(wf.wrapping_add(vs).wrapping_mul(K0)));
    fold(r.wrapping_mul(K0).wrapping_add(vs)).wrapping_mul(K2)
}

/// CityHash64 (v1.0.3)
pub fn city64(s: &[u8]) -> u64 {
    let n = s.len();
    if n <= 16 {
        return short(s);
    }
    if n <= 32 {
        return medium(s);
    }
    if n <= 64 {
        return upto64(s);
    }
    let nn = n as u64;
    // state from the last 64 bytes
    let mut x = word(s, n - 40);
    let mut y = word(s, n - 16).wrapping_add(word(s, n - 56));
    let mut z = pair(word(s, n - 48).wrapping_add(nn), word(s, n - 24));
    let mut v = lanes(s, n - 64, nn, z);
    let mut w = lanes(s, n - 32, y.wrapping_add(K1), x);
    x = x.wrapping_mul(K1).wrapping_add(word(s, 0));
    // whole 64-byte blocks from the front; the count covers all but a final partial (or full) block
    let blocks = (n - 1) / 64;
    for i in 0..blocks {
        let p = 64 * i;
        x = ror(x.wrapping_add(y).wrapping_add(v.0).wrapping_add(word(s, p + 8)), 37).wrapping_mul(K1);
        y = ror(y.wrapping_add(v.1).wrapping_add(word(s, p + 48)), 42).wrapping_mul(K1);
        x ^= w.1;
        y = y.wrapping_add(v.0).wrapping_add(word(s, p + 40));
        z = ror(z.wrapping_add(w.0), 33).wrapping_mul(K1);
        v = lanes(s, p, v.1.wrapping_mul(K1), x.wrapping_add(w.0));
        w = lanes(s, p + 32, z.wrapping_add(w.1), y.wrapping_add(word(s, p + 16)));
        std::mem::swap(&mut z, &mut x);
    }
    pair(pair(v.0, w.0).wrapping_add(fold(y).wrapping_mul(K1)).wrapping_add(z),
         pair(v.1, w.1).wrapping_add(x))
}

/// CityHash64WithSeeds
pub fn city64_with_seeds(s: &[u8], seed0: u64, seed1: u64) -> u64 {
    pair(city64(s).wrapping_sub(seed0), seed1)
}

pub fn strcode64(text: &[u8]) -> u64 {
    let seed1 = match text.first() {
        Some(&c) => ((c as u64) << 16) + text.len() as u64,
        None => 0,
    };
    let mut z = Vec::with_capacity(text.len() + 1);
    z.extend_from_slice(text);
    z.push(0);
    city64_with_seeds(&z, K2, seed1) & 0xFFFF_FFFF_FFFF
}

pub fn strcode32(text: &[u8]) -> u32 {
    strcode64(text) as u32
}

pub fn pathcode64(path: &str, ext_id: Option<u64>) -> u64 {
    let stem = path.split('.').next().unwrap_or("");
    let (body, meta) = match stem.strip_prefix("/Assets/") {
        Some(rest) => (rest, false),
        None => (stem, true),
    };
    let data = body.trim_start_matches('/').as_bytes();
    let seed1 = data.iter().rev().take(8).enumerate().fold(0u64, |acc, (i, &c)| acc | (c as u64) << (8 * i));
    let mut h = city64_with_seeds(data, K2, seed1) & 0x3_FFFF_FFFF_FFFF;
    if meta {
        h |= 1 << 50;
    }
    if let Some(e) = ext_id {
        h |= e << 51;
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    fn ramp(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + 3) & 0xFF) as u8).collect()
    }
    /// values from the previous implementation and tools/strcode.py (all length classes of city64)
    #[test]
    fn vectors() {
        assert_eq!(strcode64(b""), 0xb8a0bf169f98);
        assert_eq!(strcode32(b""), 0xbf169f98);
        assert_eq!(strcode64(b"a"), 0xb220b9bc34d5);
        assert_eq!(strcode32(b"a"), 0xb9bc34d5);
        assert_eq!(strcode64(b"ab"), 0x649f4eaa1cfb);
        assert_eq!(strcode32(b"ab"), 0x4eaa1cfb);
        assert_eq!(strcode64(b"abc"), 0x8a6b82cbece8);
        assert_eq!(strcode32(b"abc"), 0x82cbece8);
        assert_eq!(strcode64(b"abcd"), 0x20de8eaca76);
        assert_eq!(strcode32(b"abcd"), 0xe8eaca76);
        assert_eq!(strcode64(b"ObjectBrush"), 0x54e588208274);
        assert_eq!(strcode32(b"ObjectBrush"), 0x88208274);
        assert_eq!(strcode64(b"ObjectBrushBlock"), 0x92f49b87b98d);
        assert_eq!(strcode32(b"ObjectBrushBlock"), 0x9b87b98d);
        assert_eq!(strcode64(b"LightProbeSHCoefficients"), 0x64193a855820);
        assert_eq!(strcode32(b"LightProbeSHCoefficients"), 0x3a855820);
        assert_eq!(strcode64(b"numLightProbes"), 0x1cc5cea22e52);
        assert_eq!(strcode32(b"numLightProbes"), 0xcea22e52);
        assert_eq!(strcode64(b"/Assets/tpp/level/location/mafr/block_common/mafr_common_packages.fstb"), 0xcf28e75314cd);
        assert_eq!(strcode32(b"/Assets/tpp/level/location/mafr/block_common/mafr_common_packages.fstb"), 0xe75314cd);
        assert_eq!(pathcode64("/Assets/tpp/pack/location/mafr/mafr.fpk", None), 0x376b2bcad3092);
        assert_eq!(pathcode64("tpp/pack/x.fpk", None), 0x742471e0abd0d);
        assert_eq!(pathcode64("/Assets/tpp/chara/a/b.fmdl", None), 0x3aa01a1767c05);
        assert_eq!(city64(&ramp(0)), 0x9ae16a3b2f90404f);
        assert_eq!(city64(&ramp(1)), 0x5068a5b3d87a0284);
        assert_eq!(city64(&ramp(3)), 0xf6aa543ca4b8bf14);
        assert_eq!(city64(&ramp(4)), 0x9dd33b80b9fa6393);
        assert_eq!(city64(&ramp(8)), 0x37fed2dba3a300e3);
        assert_eq!(city64(&ramp(9)), 0x29b1ff616f22d6f6);
        assert_eq!(city64(&ramp(16)), 0xbdf5bcbc9b4603a4);
        assert_eq!(city64(&ramp(17)), 0xa75fe55f0d775eda);
        assert_eq!(city64(&ramp(32)), 0xd7989f47d40c660d);
        assert_eq!(city64(&ramp(33)), 0xef04fa08e8e5b8fb);
        assert_eq!(city64(&ramp(64)), 0xc1515868c51fc399);
        assert_eq!(city64(&ramp(65)), 0xacf6a69f1f078342);
        assert_eq!(city64(&ramp(128)), 0x899e1a09acbba491);
        assert_eq!(city64(&ramp(200)), 0x6099c264b552b496);
        assert_eq!(city64(&ramp(300)), 0x4954d84a69f0ea2);
    }
}
