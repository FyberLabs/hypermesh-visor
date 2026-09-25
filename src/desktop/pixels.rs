use crate::desktop::DesktopError;

pub fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, DesktopError> {
    let expected = width as usize * height as usize * 4;
    if rgba.len() != expected {
        return Err(DesktopError::Unavailable("frame size mismatch".into()));
    }
    let mut buf = Vec::new();
    let mut encoder = png::Encoder::new(&mut buf, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder
        .write_header()
        .map_err(|err| DesktopError::Unavailable(err.to_string()))?;
    writer
        .write_image_data(rgba)
        .map_err(|err| DesktopError::Unavailable(err.to_string()))?;
    writer
        .finish()
        .map_err(|err| DesktopError::Unavailable(err.to_string()))?;
    Ok(buf)
}

pub fn zpixmap_to_rgba(
    data: &[u8],
    width: u32,
    height: u32,
    bytes_per_pixel: usize,
    byte_order_lsb: bool,
    red_mask: u32,
    green_mask: u32,
    blue_mask: u32,
) -> Result<Vec<u8>, DesktopError> {
    let count = width as usize * height as usize;
    if bytes_per_pixel == 0 || data.len() < count * bytes_per_pixel {
        return Err(DesktopError::Unavailable("short image".into()));
    }
    let mut rgba = Vec::with_capacity(count * 4);
    for index in 0..count {
        let start = index * bytes_per_pixel;
        let pixel = read_pixel(&data[start..start + bytes_per_pixel], byte_order_lsb);
        rgba.push(channel(pixel, red_mask));
        rgba.push(channel(pixel, green_mask));
        rgba.push(channel(pixel, blue_mask));
        rgba.push(255);
    }
    Ok(rgba)
}

fn read_pixel(bytes: &[u8], lsb: bool) -> u32 {
    match (bytes.len(), lsb) {
        (4, true) => u32::from_le_bytes(bytes.try_into().unwrap()),
        (4, false) => u32::from_be_bytes(bytes.try_into().unwrap()),
        (3, true) => bytes[0] as u32 | ((bytes[1] as u32) << 8) | ((bytes[2] as u32) << 16),
        (3, false) => ((bytes[0] as u32) << 16) | ((bytes[1] as u32) << 8) | bytes[2] as u32,
        (2, true) => u16::from_le_bytes([bytes[0], bytes[1]]) as u32,
        (2, false) => u16::from_be_bytes([bytes[0], bytes[1]]) as u32,
        (1, _) => bytes[0] as u32,
        _ => 0,
    }
}

fn channel(pixel: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let width = (mask >> shift).count_ones();
    let value = (pixel & mask) >> shift;
    if width >= 8 {
        (value >> (width - 8)) as u8
    } else {
        (value << (8 - width)) as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpacks_little_endian_zpixmap() {
        let rgba = zpixmap_to_rgba(
            &[0x11, 0x22, 0x33, 0x00],
            1,
            1,
            4,
            true,
            0xff,
            0xff00,
            0xff0000,
        )
        .unwrap();
        assert_eq!(rgba, vec![0x11, 0x22, 0x33, 255]);
    }

    #[test]
    fn encodes_png() {
        let png = encode_rgba_png(1, 1, &[1, 2, 3, 255]).unwrap();
        assert!(png.starts_with(b"\x89PNG"));
    }
}
