//! BC1 (DXT1) and BC3 (DXT5) blocks decoded to RGBA8 for texture inspection.
//!
//! This is our format implementation, written from the block layout and palette
//! definitions, with no Pillow or other decoder implementation copied here.
//! See Microsoft's Direct3D block-compression documentation:
//! <https://learn.microsoft.com/en-us/windows/win32/direct3d10/d3d10-graphics-programming-guide-resources-block-compression>.
//!
//! RGB565 endpoints expand by bit replication. Interpolated byte components use
//! integer division (fractional parts discarded). This convention is explicit;
//! it does not promise identical rounding to every GPU or to the encoder's
//! floating-point evaluation decoder. sRGB values remain encoded bytes.
//! Blocks are tightly packed in row-major order, without a surface-pitch gap.

/// Decode the first BC1 image/slice. Partial edge blocks are cropped to the size.
pub fn decode_bc1(data: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    decode(data, width, height, false)
}

/// Decode the first BC3 image/slice. Extra bytes may contain subsequent slices.
pub fn decode_bc3(data: &[u8], width: usize, height: usize) -> Result<Vec<u8>, String> {
    decode(data, width, height, true)
}

fn decode(data: &[u8], width: usize, height: usize, has_alpha: bool) -> Result<Vec<u8>, String> {
    let format = if has_alpha { "BC3" } else { "BC1" };
    if width == 0 || height == 0 {
        return Err(format!("{format}: width and height must be nonzero"));
    }
    let block_bytes = if has_alpha { 16 } else { 8 };
    let blocks_wide = width.div_ceil(4);
    let blocks_high = height.div_ceil(4);
    let compressed_size = blocks_wide
        .checked_mul(blocks_high)
        .and_then(|blocks| blocks.checked_mul(block_bytes))
        .ok_or_else(|| format!("{format}: compressed size overflows for {width} x {height}"))?;
    let rgba_size = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| format!("{format}: RGBA size overflows for {width} x {height}"))?;
    if data.len() < compressed_size {
        return Err(format!(
            "{format}: truncated blocks; {width} x {height} needs {compressed_size} bytes, got {}",
            data.len()
        ));
    }
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(rgba_size)
        .map_err(|error| format!("{format}: cannot allocate RGBA output: {error}"))?;
    rgba.resize(rgba_size, 0);

    for block_y in 0..blocks_high {
        for block_x in 0..blocks_wide {
            let offset = (block_y * blocks_wide + block_x) * block_bytes;
            let block = &data[offset..offset + block_bytes];
            let colours = &block[block_bytes - 8..];
            let endpoint0 = u16::from_le_bytes([colours[0], colours[1]]);
            let endpoint1 = u16::from_le_bytes([colours[2], colours[3]]);
            let palette = colour_palette(endpoint0, endpoint1, has_alpha);
            let colour_indices = u32::from_le_bytes(colours[4..8].try_into().unwrap());
            let alpha = if has_alpha {
                alpha_palette(block[0], block[1])
            } else {
                [255; 8]
            };
            let mut alpha_indices = 0u64;
            if has_alpha {
                for (byte, &value) in block[2..8].iter().enumerate() {
                    alpha_indices |= (value as u64) << (8 * byte);
                }
            }
            for local_y in 0..4 {
                let y = block_y * 4 + local_y;
                if y >= height {
                    break;
                }
                for local_x in 0..4 {
                    let x = block_x * 4 + local_x;
                    if x >= width {
                        break;
                    }
                    let texel = local_y * 4 + local_x;
                    let mut pixel = palette[((colour_indices >> (2 * texel)) & 3) as usize];
                    if has_alpha {
                        pixel[3] = alpha[((alpha_indices >> (3 * texel)) & 7) as usize];
                    }
                    let output = (y * width + x) * 4;
                    rgba[output..output + 4].copy_from_slice(&pixel);
                }
            }
        }
    }
    Ok(rgba)
}

fn rgb565(endpoint: u16) -> [u8; 4] {
    let red = ((endpoint >> 11) & 31) as u8;
    let green = ((endpoint >> 5) & 63) as u8;
    let blue = (endpoint & 31) as u8;
    [
        (red << 3) | (red >> 2),
        (green << 2) | (green >> 4),
        (blue << 3) | (blue >> 2),
        255,
    ]
}

fn colour_palette(endpoint0: u16, endpoint1: u16, force_four_colours: bool) -> [[u8; 4]; 4] {
    let first = rgb565(endpoint0);
    let second = rgb565(endpoint1);
    if endpoint0 > endpoint1 || force_four_colours {
        [
            first,
            second,
            mix_colour(first, second, 2, 1),
            mix_colour(first, second, 1, 2),
        ]
    } else {
        [first, second, mix_colour(first, second, 1, 1), [0; 4]]
    }
}

fn mix_colour(first: [u8; 4], second: [u8; 4], first_weight: u16, second_weight: u16) -> [u8; 4] {
    std::array::from_fn(|channel| {
        ((first_weight * first[channel] as u16 + second_weight * second[channel] as u16)
            / (first_weight + second_weight)) as u8
    })
}

fn alpha_palette(first: u8, second: u8) -> [u8; 8] {
    let mut palette = [first, second, 0, 0, 0, 0, 0, 255];
    let denominator = if first > second { 7 } else { 5 };
    for step in 1..denominator {
        palette[step as usize + 1] =
            (((denominator - step) * first as u16 + step * second as u16) / denominator) as u8;
    }
    palette
}
