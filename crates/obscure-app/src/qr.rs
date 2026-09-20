//! QR code reading: from an image file and from a screenshot taken through
//! the Screenshot portal.

use std::path::Path;

/// Decodes every QR code in the image and returns their text, joined by
/// newlines (one link per line is what the import dialog expects).
pub fn decode_image_file(path: &Path) -> Result<String, String> {
    let img = image::open(path).map_err(|e| e.to_string())?.to_luma8();
    decode_luma(img)
}

pub fn decode_image_bytes(bytes: &[u8]) -> Result<String, String> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| e.to_string())?
        .to_luma8();
    decode_luma(img)
}

fn decode_luma(img: image::GrayImage) -> Result<String, String> {
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let grids = prepared.detect_grids();
    let mut texts = Vec::new();
    for grid in grids {
        if let Ok((_, content)) = grid.decode() {
            texts.push(content);
        }
    }
    if texts.is_empty() {
        Err("no QR code found".into())
    } else {
        Ok(texts.join("\n"))
    }
}

/// Takes a screenshot via the portal and decodes the QR code(s) in it.
/// The portal shows its own consent dialog; the file it returns is deleted
/// afterwards.
pub async fn decode_from_screen() -> Result<String, String> {
    use ashpd::desktop::screenshot::Screenshot;
    let response = Screenshot::request()
        .interactive(false)
        .modal(true)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .response()
        .map_err(|e| e.to_string())?;
    let uri = response.uri().to_string();
    let path = uri
        .strip_prefix("file://")
        .map(|p| {
            percent_encoding::percent_decode_str(p)
                .decode_utf8_lossy()
                .into_owned()
        })
        .ok_or_else(|| format!("unexpected screenshot uri {uri}"))?;
    let result = decode_image_file(Path::new(&path));
    let _ = std::fs::remove_file(&path);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_through_qrcode_crate() {
        let text = "vless://2b1e4a3c-6f7d-4e8a-9b0c-1d2e3f4a5b6c@example.org:443?security=none#QR";
        let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
        let img = code
            .render::<image::Luma<u8>>()
            .min_dimensions(300, 300)
            .build();
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(decode_image_bytes(&png).unwrap(), text);
        assert!(decode_image_bytes(b"not an image").is_err());
    }
}
