//! The failure kinds a host can branch on.

use std::fmt;

/// What went wrong, as a name a host can dispatch on.
///
/// The names are a public contract: the JS shell puts them on the
/// thrown `Error`'s `.name`, and callers branch on them. Add kinds
/// freely; do not rename one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorKind {
    /// The style JSON did not parse, did not build a graph, or a
    /// render-time option was not one of the values it allows.
    InvalidStyle,
    /// A brush source's `.myb` JSON did not parse.
    BrushParse,
    /// MVT bytes did not decode.
    MvtDecode,
    /// DEM bytes did not decode.
    DemDecode,
    /// Raster imagery bytes did not decode.
    RasterDecode,
    /// GeoJSON bytes did not parse.
    GeoJsonDecode,
    /// A sprite atlas or its index did not decode.
    SpriteDecode,
    /// Font bytes did not parse.
    FontParse,
    /// A glyph PBF did not decode.
    GlyphDecode,
    /// Graph evaluation failed, or produced something other than a
    /// raster.
    RenderFailed,
    /// PNG encoding failed.
    PngEncode,
    /// WebP encoding failed.
    WebpEncode,
    /// The style declares no such source, or the binding for one does
    /// not fit what the style says it is.
    UnknownSource,
    /// The heap could not grow. Raised by the shell's allocator, never
    /// from renderer code — the name lives here so every shell reports
    /// it identically.
    OutOfMemory,
}

impl ErrorKind {
    /// The wire name for this kind. Stable; hosts branch on it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::InvalidStyle => "InvalidStyle",
            Self::BrushParse => "BrushParse",
            Self::MvtDecode => "MvtDecode",
            Self::DemDecode => "DemDecode",
            Self::RasterDecode => "RasterDecode",
            Self::GeoJsonDecode => "GeoJsonDecode",
            Self::SpriteDecode => "SpriteDecode",
            Self::FontParse => "FontParse",
            Self::GlyphDecode => "GlyphDecode",
            Self::RenderFailed => "RenderFailed",
            Self::PngEncode => "PngEncode",
            Self::WebpEncode => "WebpEncode",
            Self::UnknownSource => "UnknownSource",
            Self::OutOfMemory => "OutOfMemory",
        }
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// A renderer failure: a kind a host can branch on, and a message for a
/// human.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    /// Build an error of `kind` from anything printable.
    pub fn new(kind: ErrorKind, message: impl fmt::Display) -> Self {
        Self {
            kind,
            message: message.to_string(),
        }
    }

    /// Which failure this is.
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// The wire name of [`Error::kind`].
    pub fn name(&self) -> &'static str {
        self.kind.name()
    }

    /// The human-readable detail, without the kind's name.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}
