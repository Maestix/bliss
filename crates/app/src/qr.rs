//! QR codes both ways. A link as a code, for the phone's «QR-код» to scan
//! off the screen: one SVG path of unit squares, drawn by the window at
//! whatever size. And a code read off a picture — a screenshot, a photo — for
//! a link that came as one.

use std::fmt::Write as _;

/// The code's side in modules, and its dark modules as an SVG path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Qr {
    pub side: usize,
    pub path: String,
}

/// `None` for text too long for any QR code.
pub fn of(text: &str) -> Option<Qr> {
    let code =
        qrcode::QrCode::with_error_correction_level(text.as_bytes(), qrcode::EcLevel::M).ok()?;
    let side = code.width();
    let mut path = String::new();
    for (i, color) in code.to_colors().iter().enumerate() {
        if *color == qrcode::Color::Dark {
            let _ = write!(path, "M{} {}h1v1h-1z", i % side, i / side);
        }
    }
    Some(Qr { side, path })
}

/// A picture larger than this is not opened: a code's picture is kilobytes,
/// and a decoder sized by a file's word would allocate what it says.
pub const MAX_PICTURE: usize = 20 * 1024 * 1024;
/// Nor one wider or taller than this, whatever its file weighs.
const MAX_SIDE: u32 = 8_000;

/// The text of the QR code in a picture, or why there is none, in words.
pub fn read(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_PICTURE {
        return Err(format!(
            "картинка больше {} МБ — сохраните код поменьше",
            MAX_PICTURE / 1024 / 1024
        ));
    }
    let not_a_picture =
        || "это не картинка, которую bliss умеет открыть: нужен PNG, JPEG, WebP или BMP".to_owned();
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| not_a_picture())?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SIDE);
    limits.max_image_height = Some(MAX_SIDE);
    limits.max_alloc = Some(512 * 1024 * 1024);
    reader.limits(limits);
    let picture = reader.decode().map_err(|_| not_a_picture())?.to_luma8();
    let (width, height) = picture.dimensions();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(width as usize, height as usize, |x, y| {
            picture.get_pixel(x as u32, y as u32).0[0]
        });
    prepared
        .detect_grids()
        .into_iter()
        .find_map(|grid| grid.decode().ok().map(|(_, text)| text))
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .ok_or_else(|| "на картинке не нашлось QR-кода — снимите его крупнее и ровнее".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_is_a_square_of_modules() {
        let qr = of("vless://11111111-2222-4333-8444-555555555555@a.example:443").unwrap();
        // Version 1 is 21 modules, and every version adds four.
        assert!(
            qr.side >= 21 && (qr.side - 21).is_multiple_of(4),
            "{}",
            qr.side
        );
        // The three finder squares alone are 3 × 33 dark modules.
        assert!(qr.path.matches('M').count() > 99);
        assert!(of(&"x".repeat(8000)).is_none(), "too long for any version");
    }

    /// A code bliss draws reads back off a picture of it, as a PNG.
    #[test]
    fn a_code_is_read_back_off_its_picture() {
        let link = "trojan://PPPP@a.example:443?security=tls&type=tcp#%D0%A1%D0%B5%D1%80%D0%B2%D0%B5%D1%80";
        let code = qrcode::QrCode::new(link.as_bytes()).unwrap();
        let side = code.width();
        let (scale, quiet) = (6u32, 4u32);
        let px = (side as u32 + quiet * 2) * scale;
        let colors = code.to_colors();
        let picture = image::GrayImage::from_fn(px, px, |x, y| {
            let (mx, my) = (x / scale, y / scale);
            let dark = mx >= quiet
                && my >= quiet
                && mx < side as u32 + quiet
                && my < side as u32 + quiet
                && colors[((my - quiet) as usize) * side + (mx - quiet) as usize]
                    == qrcode::Color::Dark;
            image::Luma([if dark { 0 } else { 255 }])
        });
        let mut png = Vec::new();
        picture
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(read(&png).unwrap(), link);
    }

    #[test]
    fn what_is_no_picture_or_too_big_is_said() {
        assert!(read(b"not a picture").unwrap_err().contains("не картинка"));
        let blank = image::GrayImage::from_pixel(64, 64, image::Luma([255]));
        let mut png = Vec::new();
        blank
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert!(read(&png).unwrap_err().contains("не нашлось"));
        assert!(
            read(&vec![0u8; MAX_PICTURE + 1])
                .unwrap_err()
                .contains("МБ")
        );
    }
}
