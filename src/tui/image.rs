//! Image preview support for the TUI.
//!
//! Provides terminal image rendering using sixel, kitty, or iterm2 graphics protocols.
//! Falls back to unicode half-blocks for unsupported terminals.

#![cfg(feature = "image-preview")]

use base64::Engine;
use ratatui::layout::Rect;
use ratatui::Frame;
use ratatui_image::picker::Picker;
use ratatui_image::protocol::StatefulProtocol;
use ratatui_image::StatefulImage;

use crate::model::content::ImageSource;

/// Image rendering protocol detected for the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageProtocol {
    /// Kitty Graphics Protocol.
    Kitty,
    /// iTerm2 Inline Images.
    Iterm2,
    /// Sixel graphics.
    Sixel,
    /// Unicode half-blocks fallback.
    Halfblocks,
}

impl std::fmt::Display for ImageProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Kitty => write!(f, "Kitty"),
            Self::Iterm2 => write!(f, "iTerm2"),
            Self::Sixel => write!(f, "Sixel"),
            Self::Halfblocks => write!(f, "Halfblocks"),
        }
    }
}

/// Image renderer for terminal display.
pub struct ImageRenderer {
    /// Detected protocol.
    protocol: ImageProtocol,
    /// Protocol picker from ratatui-image.
    picker: Picker,
}

impl ImageRenderer {
    /// Create a new image renderer, auto-detecting terminal capabilities.
    pub fn new() -> Self {
        let picker = Picker::from_query_stdio().unwrap_or_else(|_| {
            // Fall back to halfblocks
            Picker::halfblocks()
        });

        let protocol = Self::detect_protocol();

        Self { protocol, picker }
    }

    /// Create a new image renderer using halfblocks (no terminal graphics support needed).
    pub fn halfblocks() -> Self {
        let picker = Picker::halfblocks();
        let protocol = ImageProtocol::Halfblocks;
        Self { protocol, picker }
    }

    /// Detect the image protocol being used.
    fn detect_protocol() -> ImageProtocol {
        // Check environment variables for common terminals
        if std::env::var("KITTY_WINDOW_ID").is_ok() {
            return ImageProtocol::Kitty;
        }
        if std::env::var("ITERM_SESSION_ID").is_ok() {
            return ImageProtocol::Iterm2;
        }
        // Check for sixel support via TERM
        if let Ok(term) = std::env::var("TERM") {
            if term.contains("sixel") || term.contains("mlterm") || term.contains("xterm") {
                return ImageProtocol::Sixel;
            }
        }
        ImageProtocol::Halfblocks
    }

    /// Get the detected protocol.
    #[must_use]
    pub fn protocol(&self) -> ImageProtocol {
        self.protocol
    }

    /// Decode and prepare an image for rendering.
    ///
    /// Returns a `StatefulProtocol` that can be used with `render_image_state`.
    pub fn prepare_image(&mut self, source: &ImageSource) -> Result<StatefulProtocol, ImageError> {
        let image_data = self.decode_source(source)?;
        self.prepare_bytes(&image_data)
    }

    /// Decode image data from an ImageSource.
    fn decode_source(&self, source: &ImageSource) -> Result<Vec<u8>, ImageError> {
        match source {
            ImageSource::Base64 { data, .. } => {
                base64::engine::general_purpose::STANDARD
                    .decode(data)
                    .map_err(|e| ImageError::DecodeError(format!("Base64 decode failed: {e}")))
            }
            ImageSource::Url { url } => {
                Err(ImageError::UnsupportedSource(format!(
                    "URL images not supported in TUI: {url}"
                )))
            }
            ImageSource::File { file_id } => {
                Err(ImageError::UnsupportedSource(format!(
                    "File API images not supported: {file_id}"
                )))
            }
        }
    }

    /// Prepare image bytes for rendering.
    fn prepare_bytes(&mut self, data: &[u8]) -> Result<StatefulProtocol, ImageError> {
        use image::ImageReader;
        use std::io::Cursor;

        // Decode the image
        let reader = ImageReader::new(Cursor::new(data))
            .with_guessed_format()
            .map_err(|e| ImageError::DecodeError(format!("Failed to read image: {e}")))?;

        let dyn_image = reader
            .decode()
            .map_err(|e| ImageError::DecodeError(format!("Failed to decode image: {e}")))?;

        // Create the stateful protocol for rendering
        let protocol = self.picker.new_resize_protocol(dyn_image);

        Ok(protocol)
    }

    /// Render a prepared image state to the terminal.
    pub fn render_image_state(
        frame: &mut Frame,
        area: Rect,
        image_state: &mut StatefulProtocol,
    ) {
        let widget = StatefulImage::default();
        frame.render_stateful_widget(widget, area, image_state);
    }

    /// Convenience method to decode, prepare, and render an image in one call.
    ///
    /// Note: For repeated rendering of the same image, use `prepare_image` once
    /// and then `render_image_state` for each frame.
    pub fn render_image(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        source: &ImageSource,
    ) -> Result<(), ImageError> {
        let mut image_state = self.prepare_image(source)?;
        Self::render_image_state(frame, area, &mut image_state);
        Ok(())
    }

    /// Check if image preview is supported.
    #[must_use]
    pub fn is_supported(&self) -> bool {
        // All protocols including halfblocks work
        true
    }

    /// Get a description of the current image rendering capabilities.
    #[must_use]
    pub fn capabilities_string(&self) -> String {
        format!("Image preview: {} protocol", self.protocol)
    }
}

impl Default for ImageRenderer {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors that can occur during image rendering.
#[derive(Debug, thiserror::Error)]
pub enum ImageError {
    /// Failed to decode image data.
    #[error("Image decode error: {0}")]
    DecodeError(String),

    /// Unsupported image source type.
    #[error("Unsupported image source: {0}")]
    UnsupportedSource(String),

    /// Rendering error.
    #[error("Image render error: {0}")]
    RenderError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_display() {
        assert_eq!(ImageProtocol::Kitty.to_string(), "Kitty");
        assert_eq!(ImageProtocol::Sixel.to_string(), "Sixel");
        assert_eq!(ImageProtocol::Halfblocks.to_string(), "Halfblocks");
    }
}
