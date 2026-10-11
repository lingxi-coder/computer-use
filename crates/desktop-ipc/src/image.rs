//! A separate, bounded PNG channel. Control and stop messages never use it.
//! Wire: CUIMG001, u32 metadata bytes, u32 PNG bytes (both big endian),
//! FrameMeta JSON, then PNG bytes. No base64, paths, or mutable buffer handles.

use std::io::{self, Read, Write};
use std::sync::Arc;

use interaction_contracts::desktop::source::{FrameMeta, FrameStatus};
use sha2::{Digest, Sha256};

pub const MAGIC: &[u8; 8] = b"CUIMG001";
pub const AUTH_MAGIC: &[u8; 8] = b"CUIAUTH1";
pub const AUTH_OK: &[u8; 8] = b"CUIREADY";
pub const MAX_METADATA: usize = 64 * 1024;
pub const MAX_IMAGE: usize = 12 * 1024 * 1024;
pub const MAX_PIXELS: u64 = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct ImageFrame {
    pub meta: FrameMeta,
    pub bytes: Arc<[u8]>,
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

pub fn validate_meta(meta: &FrameMeta) -> io::Result<()> {
    let bounded =
        |s: &str| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control);
    let rect = |r: interaction_contracts::desktop::source::Rect, empty: bool| {
        [r.x, r.y, r.width, r.height].iter().all(|v| v.is_finite())
            && r.width >= 0.0
            && r.height >= 0.0
            && (empty || (r.width > 0.0 && r.height > 0.0))
    };
    let Some(stream) = meta.window_geometry() else {
        return Err(invalid("PNG has no native stream provenance"));
    };
    if meta.status != FrameStatus::Complete
        || meta.session_epoch == 0
        || meta.source_sequence == 0
        || !bounded(&meta.frame_id)
        || !bounded(&meta.surface.id)
        || meta.surface.epoch == 0
        || !bounded(&meta.clock_domain)
        || meta.watch_id().is_none_or(|id| !bounded(id))
        || !bounded(stream.window_id)
        || meta.image_frame_id().is_none_or(|id| !bounded(id))
        || stream.pid == 0
        || meta.width_px == 0
        || meta.height_px == 0
        || meta.width_px > 4096
        || meta.height_px > 4096
        || u64::from(meta.width_px) * u64::from(meta.height_px) > MAX_PIXELS
        || meta.byte_length < 57
        || meta.byte_length > MAX_IMAGE as u64
        || meta.geometry_revision == 0
        || meta.checksum.len() != 64
        || !meta
            .checksum
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !meta.scale_factor.is_finite()
        || meta.scale_factor <= 0.0
        || !stream.content_scale.is_finite()
        || stream.content_scale <= 0.0
        || !rect(stream.window_frame, false)
        || !rect(stream.content_rect, false)
    {
        return Err(invalid(
            "PNG metadata exceeds bounds or lacks complete provenance",
        ));
    }
    if let Some(stream) = meta.stream_info() {
        if stream.dirty_rects.len() > 256
            || stream.dirty_rects.iter().any(|r| !rect(*r, true))
            || stream.callback_ms < meta.source_ms
            || stream.display_time_ticks == 0
            || stream.display_time_ns == 0
            || stream.received_uptime_ns < stream.display_time_ns
            || stream.source_clock != "mach_absolute"
        {
            return Err(invalid("PNG lacks complete native stream timing"));
        }
    } else if let Some(capture) = meta.capture_info() {
        capture
            .validate()
            .map_err(|_| invalid("PNG has invalid single-capture provenance"))?;
        if meta.source_ms != capture.capture_started_ms {
            return Err(invalid(
                "PNG capture source time differs from its start bound",
            ));
        }
    } else {
        return Err(invalid("PNG has no typed native provenance"));
    }
    Ok(())
}

/// Inspect the compressed container before a consumer decompresses it. Only a
/// single non-animated 8-bit RGB/RGBA image is supported; expanded RGBA is <=16MiB.
pub fn validate_png(meta: &FrameMeta, bytes: &[u8]) -> io::Result<()> {
    validate_png_with_check(meta, bytes, &mut || Ok(()))
}

pub(crate) fn validate_png_with_check(
    meta: &FrameMeta,
    bytes: &[u8],
    checkpoint: &mut impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    checkpoint()?;
    validate_meta(meta)?;
    if bytes.len() as u64 != meta.byte_length
        || bytes.len() < 57
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
    {
        return Err(invalid("PNG signature or length does not match metadata"));
    }
    let mut offset = 8;
    let mut data = false;
    let mut end = false;
    while offset + 12 <= bytes.len() {
        checkpoint()?;
        let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let next = offset
            .checked_add(12)
            .and_then(|n| n.checked_add(length))
            .filter(|n| *n <= bytes.len())
            .ok_or_else(|| invalid("PNG chunk exceeds the bounded container"))?;
        let kind = &bytes[offset + 4..offset + 8];
        if offset == 8 {
            if kind != b"IHDR" || length != 13 {
                return Err(invalid("PNG first chunk is not IHDR"));
            }
            let head = &bytes[offset + 8..offset + 21];
            if u32::from_be_bytes(head[..4].try_into().unwrap()) != meta.width_px
                || u32::from_be_bytes(head[4..8].try_into().unwrap()) != meta.height_px
                || head[8] != 8
                || !matches!(head[9], 2 | 6)
                || head[10] != 0
                || head[11] != 0
                || head[12] != 0
            {
                return Err(invalid(
                    "PNG dimensions or decoding format do not match metadata",
                ));
            }
        } else if kind == b"IHDR" || matches!(kind, b"acTL" | b"fcTL" | b"fdAT") {
            return Err(invalid("PNG contains another image or animation"));
        } else if kind == b"IDAT" {
            data = true;
        } else if kind == b"IEND" {
            if length != 0 || next != bytes.len() {
                return Err(invalid("PNG has trailing content"));
            }
            end = true;
        }
        offset = next;
    }
    if offset != bytes.len() || !data || !end {
        return Err(invalid("PNG is incomplete or its checksum does not match"));
    }
    let mut checksum = Sha256::new();
    for chunk in bytes.chunks(64 * 1024) {
        checkpoint()?;
        checksum.update(chunk);
    }
    if format!("{:x}", checksum.finalize()) != meta.checksum {
        return Err(invalid("PNG is incomplete or its checksum does not match"));
    }
    Ok(())
}

impl ImageFrame {
    pub fn new(meta: FrameMeta, bytes: Arc<[u8]>) -> io::Result<Self> {
        validate_png(&meta, &bytes)?;
        Ok(Self { meta, bytes })
    }

    pub fn write_to(&self, out: &mut impl Write) -> io::Result<()> {
        self.write_to_with_check(out, || Ok(()))
    }

    /// The sender's cancellation/deadline is checked during container and
    /// checksum work, before any packet prefix is published. It cannot
    /// interrupt a blocking `Write`; callers must use time-bounded IO and
    /// discard the output stream after an error. The wire is unchanged.
    pub fn write_to_with_check(
        &self,
        out: &mut impl Write,
        mut checkpoint: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        validate_png_with_check(&self.meta, &self.bytes, &mut checkpoint)?;
        self.write_validated_to_with_check(out, checkpoint)
    }

    /// Write an image whose PNG was already validated by the caller. This is
    /// crate-private so a compound packet can validate before its outer prefix
    /// without hashing the bounded image a second time.
    pub(crate) fn write_validated_to_with_check(
        &self,
        out: &mut impl Write,
        mut checkpoint: impl FnMut() -> io::Result<()>,
    ) -> io::Result<()> {
        let header = serde_json::to_vec(&self.meta).map_err(io::Error::other)?;
        if header.len() > MAX_METADATA {
            return Err(invalid("PNG metadata too large"));
        }
        checkpoint()?;
        out.write_all(MAGIC)?;
        out.write_all(&(header.len() as u32).to_be_bytes())?;
        out.write_all(&(self.bytes.len() as u32).to_be_bytes())?;
        out.write_all(&header)?;
        out.write_all(&self.bytes)
    }
}

pub fn read_from(input: &mut impl Read) -> io::Result<ImageFrame> {
    read_from_with_check(input, || Ok(()))
}

/// Read and validate one bounded image packet, checking cancellation/deadline
/// between reads and throughout PNG chunk and checksum work. The callback
/// cannot interrupt a blocking `Read`; callers must use time-bounded IO and
/// discard the stream after an error.
pub fn read_from_with_check<R, C>(input: &mut R, mut checkpoint: C) -> io::Result<ImageFrame>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    let (meta, bytes) = {
        let mut input = CheckpointReader {
            input,
            checkpoint: &mut checkpoint,
        };
        let mut prefix = [0u8; 16];
        input.read_exact(&mut prefix)?;
        let metadata = u32::from_be_bytes(prefix[8..12].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(prefix[12..16].try_into().unwrap()) as usize;
        if &prefix[..8] != MAGIC
            || !(2..=MAX_METADATA).contains(&metadata)
            || !(57..=MAX_IMAGE).contains(&length)
        {
            return Err(invalid("PNG packet prefix exceeds bounds"));
        }
        let mut header = vec![0; metadata];
        input.read_exact(&mut header)?;
        let meta: FrameMeta = serde_json::from_slice(&header).map_err(io::Error::other)?;
        validate_meta(&meta)?;
        if meta.byte_length != length as u64 {
            return Err(invalid("PNG packet length disagrees with metadata"));
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        (meta, bytes)
    };
    validate_png_with_check(&meta, &bytes, &mut checkpoint)?;
    Ok(ImageFrame {
        meta,
        bytes: bytes.into(),
    })
}

struct CheckpointReader<'a, R, C> {
    input: &'a mut R,
    checkpoint: &'a mut C,
}

impl<R, C> Read for CheckpointReader<'_, R, C>
where
    R: Read,
    C: FnMut() -> io::Result<()>,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (self.checkpoint)()?;
        let read = self.input.read(buffer)?;
        (self.checkpoint)()?;
        Ok(read)
    }

    fn read_exact(&mut self, mut buffer: &mut [u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            (self.checkpoint)()?;
            let length = match self.input.read(buffer) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(length) => length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            (self.checkpoint)()?;
            buffer = &mut buffer[length..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn cancellation_checkpoint_is_not_retried_as_a_syscall_interruption() {
        let mut calls = 0;
        let error = read_from_with_check(
            &mut Cursor::new(include_bytes!(
                "../../../contracts/desktop-v1/image-channel/packet.bin"
            )),
            || {
                calls += 1;
                if calls >= 2 {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(calls, 2);
    }
}
