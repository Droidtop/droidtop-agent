//! The window's and the tray's icon, drawn here rather than shipped as an
//! image: two linked dots (a computer and a handheld kept in step) on a
//! rounded square.

/// The icon as RGBA, [`size`] pixels square.
pub fn rgba(size: u32) -> (Vec<u8>, u32) {
    let s = size as f32;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    let background = [0x2e, 0x6b, 0x5e];
    let radius = s * 0.22;
    let dots = [(s * 0.32, s * 0.5), (s * 0.68, s * 0.5)];
    let dot = s * 0.14;
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            // Coverage of the rounded square, softened over one pixel.
            let dx = (radius - px).max(px - (s - radius)).max(0.0);
            let dy = (radius - py).max(py - (s - radius)).max(0.0);
            let outside = (dx * dx + dy * dy).sqrt() - radius;
            let alpha = (0.5 - outside).clamp(0.0, 1.0);
            // The dots and the bar between them.
            let near_dot = dots.iter().map(|(cx, cy)| ((px - cx).powi(2) + (py - cy).powi(2)).sqrt() - dot).fold(f32::MAX, f32::min);
            let near_bar = if px > dots[0].0 && px < dots[1].0 { (py - s * 0.5).abs() - s * 0.045 } else { f32::MAX };
            let white = (0.5 - near_dot.min(near_bar)).clamp(0.0, 1.0);
            let mix = |c: u8| (c as f32 * (1.0 - white) + 255.0 * white).round() as u8;
            out.extend_from_slice(&[mix(background[0]), mix(background[1]), mix(background[2]), (alpha * 255.0).round() as u8]);
        }
    }
    (out, size)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_icon_is_square_and_its_corners_are_clear() {
        let (rgba, size) = super::rgba(32);
        assert_eq!(rgba.len(), (size * size * 4) as usize);
        assert_eq!(rgba[3], 0, "the top-left corner is transparent");
        let centre = ((16 * size + 16) * 4) as usize;
        assert_eq!(rgba[centre + 3], 255);
    }
}
