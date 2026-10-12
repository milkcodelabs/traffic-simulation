//! Wire types for the desktop shell: ingestion commands, the static viewport blob,
//! and the per-frame link-state blob.

mod frame;
mod geometry;
mod protocol;

pub use frame::{
    decode_frame, encode_frame, encode_keepalive, top_bottlenecks, Bottleneck, DecodedFrame,
    FrameHeader, FRAME_HEADER_BYTES, FRAME_MAGIC, FRAME_VERSION,
};
pub use geometry::{encode_viewport, VIEWPORT_MAGIC};
pub use protocol::{BBoxDto, IngestReport, IngestRequestDto, PresetDto};
