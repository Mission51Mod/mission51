//! SPDX-License-Identifier: MIT OR Apache-2.0
//! Render the editor's existing procedural icon into a Windows ICO asset.
use std::fs;
use std::io;
use std::path::Path;

const SIZE: usize = 64;

fn pixel(x: usize, y: usize) -> [u8; 4] {
    let center_x = x as f32 + 0.5;
    let center_y = y as f32 + 0.5;
    let corner_radius = 12.0;
    let rounded_x = center_x.clamp(corner_radius, SIZE as f32 - corner_radius);
    let rounded_y = center_y.clamp(corner_radius, SIZE as f32 - corner_radius);
    let distance = ((center_x - rounded_x).powi(2) + (center_y - rounded_y).powi(2)).sqrt();
    let alpha = ((corner_radius + 0.5 - distance).clamp(0.0, 1.0) * 255.0) as u8;
    let letter = (18..28).contains(&x) && (14..50).contains(&y)
        || (18..46).contains(&x) && (14..23).contains(&y)
        || (18..40).contains(&x) && (29..37).contains(&y);
    let [red, green, blue] = if letter {
        [0x1a, 0x1c, 0x20]
    } else {
        [0xe8, 0x9a, 0x3c]
    };
    [blue, green, red, alpha]
}

fn icon_bytes() -> Vec<u8> {
    let mask_row_bytes = SIZE.div_ceil(32) * 4;
    let bitmap_size = SIZE * SIZE * 4 + mask_row_bytes * SIZE;
    let resource_size = 40 + bitmap_size;
    let mut output = Vec::with_capacity(22 + resource_size);
    // ICO header and one directory entry.
    for value in [0u16, 1, 1] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    output.extend_from_slice(&[SIZE as u8, SIZE as u8, 0, 0]);
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&32u16.to_le_bytes());
    output.extend_from_slice(&(resource_size as u32).to_le_bytes());
    output.extend_from_slice(&22u32.to_le_bytes());
    // BITMAPINFOHEADER: icon height includes the colour image and AND mask.
    output.extend_from_slice(&40u32.to_le_bytes());
    output.extend_from_slice(&(SIZE as i32).to_le_bytes());
    output.extend_from_slice(&((SIZE * 2) as i32).to_le_bytes());
    output.extend_from_slice(&1u16.to_le_bytes());
    output.extend_from_slice(&32u16.to_le_bytes());
    for value in [0u32, bitmap_size as u32, 0, 0, 0, 0] {
        output.extend_from_slice(&value.to_le_bytes());
    }
    for y in (0..SIZE).rev() {
        for x in 0..SIZE {
            output.extend_from_slice(&pixel(x, y));
        }
    }
    for y in (0..SIZE).rev() {
        let mut row = vec![0u8; mask_row_bytes];
        for x in 0..SIZE {
            if pixel(x, y)[3] == 0 {
                row[x / 8] |= 1 << (7 - x % 8);
            }
        }
        output.extend_from_slice(&row);
    }
    output
}

fn main() -> io::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .ok_or_else(|| io::Error::other("usage: icon <output.ico>"))?;
    fs::write(Path::new(&output), icon_bytes())
}
