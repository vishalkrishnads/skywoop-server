//! Dominant color extraction for logos/screenshots.

use std::collections::HashMap;

use anyhow::Context;
use image::imageops::FilterType;

/// Return up to `n` dominant colors as `#rrggbb`, most frequent first.
/// Pixels are bucketed by rounding each channel to multiples of 32; fully transparent
/// pixels are ignored.
pub fn dominant_colors(png_or_jpeg: &[u8], n: usize) -> anyhow::Result<Vec<String>> {
    let img = image::load_from_memory(png_or_jpeg).context("decode image")?;
    let small = img.resize_exact(64, 64, FilterType::Triangle).to_rgba8();

    let mut counts: HashMap<(u8, u8, u8), u32> = HashMap::new();
    for px in small.pixels() {
        let [r, g, b, a] = px.0;
        if a < 128 {
            continue;
        }
        *counts.entry((bucket(r), bucket(g), bucket(b))).or_default() += 1;
    }

    let mut entries: Vec<_> = counts.into_iter().collect();
    // Deterministic: count desc, then color asc.
    entries.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    Ok(entries
        .into_iter()
        .take(n)
        .map(|((r, g, b), _)| format!("#{r:02x}{g:02x}{b:02x}"))
        .collect())
}

fn bucket(c: u8) -> u8 {
    (((c as u16 + 16) / 32) * 32).min(255) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, ImageFormat, Rgb};
    use std::io::Cursor;

    #[test]
    fn finds_dominant_color() {
        let img = ImageBuffer::from_fn(100, 100, |x, _| {
            if x < 70 {
                Rgb([255u8, 0, 0])
            } else {
                Rgb([0u8, 0, 255])
            }
        });
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, ImageFormat::Png).expect("encode");
        let colors = dominant_colors(buf.get_ref(), 2).expect("palette");
        assert_eq!(colors, vec!["#ff0000".to_string(), "#0000ff".to_string()]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(dominant_colors(b"nope", 3).is_err());
    }
}
