use crate::attachment_geometry::{IMAGE_TYPES, MAX_UPLOAD_BYTES};
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadedFile {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub file_name: String,
    pub mime_type: String,
    pub size: u64,
    pub path: String,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageData {
    pub data: String,
    pub mime_type: String,
}
#[derive(Clone, Debug)]
pub enum ReadyAttachment {
    Image(Arc<ImageData>),
    File(Arc<UploadedFile>),
}

pub struct DraftAttachment {
    pub id: String,
    pub path: PathBuf,
    pub file_name: String,
    pub as_image: bool,
    pub ready: Option<ReadyAttachment>,
    pub error: Option<String>,
    pub cancelled: Arc<AtomicBool>,
}
impl DraftAttachment {
    pub fn new(path: PathBuf) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            file_name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path,
            as_image: false,
            ready: None,
            error: None,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
}
impl Drop for DraftAttachment {
    fn drop(&mut self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
}

pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}
pub struct SelectedFile {
    pub file: File,
    pub file_name: String,
    pub mime_type: String,
    pub size: u64,
    pub modified_at: String,
}
pub enum Prepared {
    Image(Arc<ImageData>, Option<Thumbnail>),
    File(SelectedFile),
}

pub fn prepare(path: &Path, as_image: bool) -> Result<Prepared> {
    ensure!(
        std::fs::metadata(path)
            .context("Cannot inspect selected file")?
            .is_file(),
        "Select a regular file"
    );
    let mut file = File::open(path).context("Cannot open selected file")?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Select a regular file");
    ensure!(
        metadata.len() <= MAX_UPLOAD_BYTES,
        "File exceeds the 50MB upload limit"
    );
    let name = path
        .file_name()
        .context("Selected file has no name")?
        .to_string_lossy()
        .into_owned();
    let extension = path
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let mime = IMAGE_TYPES
        .iter()
        .find(|(ext, _)| *ext == extension)
        .map(|(_, mime)| *mime);
    if as_image {
        let mime = mime.context("Select a supported raster image")?;
        let mut bytes = Vec::with_capacity(usize::try_from(metadata.len())?);
        (&mut file)
            .take(MAX_UPLOAD_BYTES + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_UPLOAD_BYTES,
            "File exceeds the 50MB upload limit"
        );
        let thumbnail = thumbnail(&bytes);
        Ok(Prepared::Image(
            Arc::new(ImageData {
                data: STANDARD.encode(&bytes),
                mime_type: mime.into(),
            }),
            thumbnail,
        ))
    } else {
        let modified_at: chrono::DateTime<chrono::Utc> = metadata
            .modified()
            .context("Cannot read file modification time")?
            .into();
        Ok(Prepared::File(SelectedFile {
            file,
            file_name: name,
            mime_type: mime.unwrap_or("application/octet-stream").into(),
            size: metadata.len(),
            modified_at: modified_at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        }))
    }
}
fn thumbnail(bytes: &[u8]) -> Option<Thumbnail> {
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    // Unsupported formats still attach; only their optional thumbnail is unavailable.
    let image = reader.decode().ok()?;
    let image = image
        .thumbnail(96.min(image.width()), 96.min(image.height()))
        .to_rgba8();
    Some(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

pub fn transfer_frame(opcode: u8, request_id: &str, body: &[u8]) -> Result<Vec<u8>> {
    ensure!(
        !request_id.is_empty() && request_id.len() <= 255,
        "Invalid file transfer request ID"
    );
    ensure!(
        matches!(opcode, 0x10..=0x12),
        "Invalid file transfer opcode"
    );
    let mut bytes = Vec::with_capacity(body.len() + request_id.len() + 4);
    bytes.extend_from_slice(&[opcode, request_id.len() as u8]);
    bytes.extend_from_slice(request_id.as_bytes());
    if opcode == 0x10 {
        bytes.extend_from_slice(
            &u16::try_from(body.len())
                .context("File metadata is too large")?
                .to_be_bytes(),
        );
    }
    if opcode == 0x12 {
        ensure!(body.is_empty(), "File end cannot carry a payload");
    }
    bytes.extend_from_slice(body);
    Ok(bytes)
}

pub fn preview(data: &ImageData) -> Result<Thumbnail> {
    let bytes = STANDARD
        .decode(&data.data)
        .context("Cannot decode image attachment")?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().context("Cannot load this image format")?;
    let image = image
        .thumbnail(2048.min(image.width()), 2048.min(image.height()))
        .to_rgba8();
    Ok(Thumbnail {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("paseo-native-attachments-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }
    #[test]
    fn file_transfer_header_matches_the_existing_big_endian_protocol() {
        assert_eq!(
            transfer_frame(0x10, "a", b"{}").unwrap(),
            vec![0x10, 1, b'a', 0, 2, b'{', b'}']
        );
        assert_eq!(
            transfer_frame(0x11, "a", &[0, 255]).unwrap(),
            vec![0x11, 1, b'a', 0, 255]
        );
        assert_eq!(transfer_frame(0x12, "a", &[]).unwrap(), vec![0x12, 1, b'a']);
        assert!(transfer_frame(0x12, "a", b"unexpected").is_err());
        assert!(transfer_frame(0x10, "a", &vec![0; 65536]).is_err());
        assert!(transfer_frame(0x11, "", &[]).is_err());
    }
    #[test]
    fn real_files_reject_size_and_kind_errors_before_uploading() {
        let fixture = Fixture::new();
        let path = fixture.0.join("large.txt");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_UPLOAD_BYTES + 1).unwrap();
        assert!(
            prepare(&path, false)
                .err()
                .unwrap()
                .to_string()
                .contains("50MB")
        );
        file.set_len(0).unwrap();
        match prepare(&path, false).unwrap() {
            Prepared::File(file) => assert_eq!(file.size, 0),
            _ => panic!("expected file"),
        }
        assert!(prepare(&path, true).is_err());
        assert!(prepare(&fixture.0, false).is_err());
        let attachment = DraftAttachment::new(path);
        let cancelled = attachment.cancelled.clone();
        drop(attachment);
        assert!(cancelled.load(Ordering::Relaxed));
    }
    #[test]
    fn image_payload_and_bounded_preview_retain_the_original_png_bytes() {
        let fixture = Fixture::new();
        let path = fixture.0.join("图像.PNG");
        let image = image::RgbaImage::from_pixel(4, 2, image::Rgba([255, 32, 64, 255]));
        image.save(&path).unwrap();
        let original = std::fs::read(&path).unwrap();
        match prepare(&path, true).unwrap() {
            Prepared::Image(data, thumb) => {
                assert_eq!(data.mime_type, "image/png");
                assert_eq!(STANDARD.decode(&data.data).unwrap(), original);
                let thumb = thumb.unwrap();
                assert_eq!((thumb.width, thumb.height), (4, 2));
                assert_eq!(thumb.rgba.len(), 32);
                let full = preview(&data).unwrap();
                assert_eq!((full.width, full.height), (4, 2));
            }
            _ => panic!("expected image"),
        }
    }
}
