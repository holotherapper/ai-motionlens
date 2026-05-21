//! Artifact storage for full-resolution frame images.
//!
//! Frames are stored on disk under a per-session directory. The wire-level
//! response carries a short `motionlens://artifacts/<session>/<filename>` URI plus a
//! ~256px-wide base64-inlined thumbnail. The full PNG is read on demand via
//! the MCP `resources/read` flow (or directly via the resolved local path in
//! CLI mode).
//!
//! Default storage root:
//! - `$AI_MOTIONLENS_ARTIFACTS_DIR` if set
//! - else `dirs::cache_dir()/ai-motionlens/artifacts/`
//! - else `$TMPDIR/ai-motionlens/artifacts/`

use std::io::Cursor;
use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use image::{DynamicImage, ImageFormat as ImgFormat, ImageReader};
use tokio::fs;

use crate::error::{Error, Result};
use crate::model::{ArtifactId, ImageFormat, SessionId, VideoFormat};

const THUMBNAIL_WIDTH: u32 = 256;
const URI_SCHEME: &str = "motionlens";

#[derive(Debug, Clone)]
pub struct StoredArtifact {
    pub artifact_id: ArtifactId,
    pub uri: String,
    pub path: PathBuf,
    pub thumbnail_base64: Option<String>,
    pub width: u32,
    pub height: u32,
    pub format: ImageFormat,
}

#[derive(Debug, Clone)]
pub struct StoredVideoArtifact {
    pub artifact_id: ArtifactId,
    pub uri: String,
    pub path: PathBuf,
    pub format: VideoFormat,
}

pub struct ArtifactStore {
    root: PathBuf,
    session: String,
}

impl ArtifactStore {
    pub fn for_session(session_id: &SessionId) -> Result<Self> {
        let session = session_id.as_str().to_string();
        let root = resolve_root()?.join(&session);
        Ok(Self { root, session })
    }

    pub async fn ensure_root(&self) -> Result<()> {
        fs::create_dir_all(&self.root).await?;
        Ok(())
    }

    pub fn root_path(&self) -> &Path {
        &self.root
    }

    fn artifact_uri(&self, filename: &str) -> String {
        format!("{URI_SCHEME}://artifacts/{}/{}", self.session, filename)
    }

    pub async fn save(
        &self,
        png_or_jpeg_bytes: &[u8],
        format: ImageFormat,
        thumbnail: bool,
    ) -> Result<StoredArtifact> {
        let artifact_id = ArtifactId::new();
        let filename = format!("{}.{}", artifact_id.as_str(), extension_for(format));
        let path = self.root.join(&filename);
        fs::write(&path, png_or_jpeg_bytes).await?;

        let (width, height, thumbnail_base64) = if thumbnail {
            let img = decode_image(png_or_jpeg_bytes, format)?;
            let (w, h) = (img.width(), img.height());
            (w, h, Some(encode_thumbnail_base64(img, format)?))
        } else {
            let (w, h) = decode_dimensions(png_or_jpeg_bytes, format)?;
            (w, h, None)
        };

        Ok(StoredArtifact {
            artifact_id,
            uri: self.artifact_uri(&filename),
            path,
            thumbnail_base64,
            width,
            height,
            format,
        })
    }

    /// Resolve a `motionlens://artifacts/<session>/<filename>` URI to its
    /// on-disk path under the configured artifacts root. Used by the MCP
    /// `resources/read` handler to serve full-resolution blobs to clients
    /// that don't have file-system access to the agent's machine.
    pub fn resolve_uri(uri: &str) -> Result<PathBuf> {
        let prefix = format!("{URI_SCHEME}://artifacts/");
        let rest = uri
            .strip_prefix(prefix.as_str())
            .ok_or_else(|| Error::invalid(format!("not a motionlens artifact URI: {uri}")))?;
        let mut parts = rest.splitn(2, '/');
        let session = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::invalid(format!("URI missing session segment: {uri}")))?;
        let filename = parts
            .next()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::invalid(format!("URI missing filename segment: {uri}")))?;
        // Path traversal guard: filenames are uuid-shaped, sessions are id-
        // prefixed. Reject anything containing `..` or extra separators.
        if session.contains("..")
            || session.contains('/')
            || session.contains('\\')
            || filename.contains("..")
            || filename.contains('/')
            || filename.contains('\\')
        {
            return Err(Error::invalid(format!(
                "suspicious motionlens artifact URI: {uri}"
            )));
        }
        let root = resolve_root()?;
        Ok(root.join(session).join(filename))
    }

    /// Save a video container (GIF / APNG). The bytes are written verbatim
    /// using the container's natural file extension. Unlike `save`, this does
    /// not attempt to decode the bytes -- the caller already knows the
    /// dimensions because it built the frames.
    pub async fn save_video(
        &self,
        bytes: &[u8],
        format: VideoFormat,
    ) -> Result<StoredVideoArtifact> {
        let artifact_id = ArtifactId::new();
        let filename = format!("{}.{}", artifact_id.as_str(), video_extension(format));
        let path = self.root.join(&filename);
        fs::write(&path, bytes).await?;

        Ok(StoredVideoArtifact {
            artifact_id,
            uri: self.artifact_uri(&filename),
            path,
            format,
        })
    }
}

fn video_extension(format: VideoFormat) -> &'static str {
    match format {
        VideoFormat::Gif => "gif",
        VideoFormat::Apng => "apng",
    }
}

fn resolve_root() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("AI_MOTIONLENS_ARTIFACTS_DIR") {
        return Ok(PathBuf::from(p));
    }
    if let Some(cache) = dirs::cache_dir() {
        return Ok(cache.join("ai-motionlens").join("artifacts"));
    }
    Ok(std::env::temp_dir().join("ai-motionlens").join("artifacts"))
}

fn extension_for(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
    }
}

fn img_format_of(format: ImageFormat) -> ImgFormat {
    match format {
        ImageFormat::Png => ImgFormat::Png,
        ImageFormat::Jpeg => ImgFormat::Jpeg,
    }
}

fn decode_dimensions(bytes: &[u8], format: ImageFormat) -> Result<(u32, u32)> {
    ImageReader::with_format(Cursor::new(bytes), img_format_of(format))
        .into_dimensions()
        .map_err(|e| Error::invalid(format!("failed to decode image dimensions: {e}")))
}

fn decode_image(bytes: &[u8], format: ImageFormat) -> Result<DynamicImage> {
    ImageReader::with_format(Cursor::new(bytes), img_format_of(format))
        .decode()
        .map_err(|e| Error::invalid(format!("failed to decode image: {e}")))
}

/// Encode the thumbnail in the same wire format the caller requested for the
/// full image, so JPEG-requested frames don't unexpectedly carry a PNG
/// thumbnail.
fn encode_thumbnail_base64(img: DynamicImage, format: ImageFormat) -> Result<String> {
    let thumb = if img.width() > THUMBNAIL_WIDTH {
        let ratio = THUMBNAIL_WIDTH as f32 / img.width() as f32;
        let target_h = ((img.height() as f32) * ratio).round().max(1.0) as u32;
        img.thumbnail_exact(THUMBNAIL_WIDTH, target_h)
    } else {
        img
    };

    let mut buf: Vec<u8> = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut buf), img_format_of(format))
        .map_err(|e| Error::invalid(format!("failed to encode thumbnail: {e}")))?;
    Ok(BASE64_STANDARD.encode(&buf))
}
