//! A bounded image packet tied to one outstanding computer-frame request.
//!
//! Wire: `CUCIMG01`, a big-endian u32 fence-JSON length, the validated
//! `ComputerFence` JSON, then the existing `CUIMG001` ImageFrame packet.
//! The frame bytes remain binary; this codec adds correlation and deadline
//! checks without putting image data on the desktop control protocol.

use std::io::{self, Read, Write};

use interaction_contracts::desktop::computer::{ComputerFence, ComputerFrameRequest};

use crate::image::{self, ImageFrame};

pub const MAGIC: &[u8; 8] = b"CUCIMG01";
pub const MAX_FENCE_BYTES: usize = 16 * 1024;
const PREFIX_BYTES: usize = 12;

#[derive(Debug, Clone)]
pub struct ComputerImagePacket {
    pub fence: ComputerFence,
    pub image: ImageFrame,
}

fn invalid(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn request_is_valid(request: &ComputerFrameRequest, now_ms: u64) -> io::Result<()> {
    request
        .validate()
        .map_err(|_| invalid("invalid computer frame request"))?;
    if now_ms >= request.fence.deadline_ms {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "computer frame request expired",
        ));
    }
    image::validate_meta(&request.expected_frame)
}

fn live(request: &ComputerFrameRequest, now_ms: &mut impl FnMut() -> u64) -> io::Result<()> {
    request_is_valid(request, now_ms())
}

impl ComputerImagePacket {
    /// Write a frame only when both its complete metadata and fence match the
    /// outstanding request. The callback checks cancellation/deadline during
    /// PNG work and between writes, but cannot interrupt a blocking `Write`.
    /// PNG validation happens before the first output byte. Later IO errors
    /// can leave a partial packet, so callers must use time-bounded IO and
    /// discard the output stream after any error.
    pub fn write_to_with_check<W, N, C>(
        &self,
        request: &ComputerFrameRequest,
        out: &mut W,
        mut now_ms: N,
        mut checkpoint: C,
    ) -> io::Result<()>
    where
        W: Write,
        N: FnMut() -> u64,
        C: FnMut() -> io::Result<()>,
    {
        request_is_valid(request, now_ms())?;
        if self.fence != request.fence {
            return Err(invalid("computer image fence does not match the request"));
        }
        if self.image.meta != request.expected_frame {
            return Err(invalid(
                "computer image metadata does not match the requested frame",
            ));
        }

        let mut check = || {
            checkpoint()?;
            live(request, &mut now_ms)
        };
        // Never publish an outer fence prefix for invalid, cancelled, or
        // expired image validation; this performs the checksum only once.
        image::validate_png_with_check(&self.image.meta, &self.image.bytes, &mut check)?;

        let fence = serde_json::to_vec(&self.fence).map_err(io::Error::other)?;
        if !(2..=MAX_FENCE_BYTES).contains(&fence.len()) {
            return Err(invalid("computer image fence exceeds bounds"));
        }
        let fence_len = u32::try_from(fence.len())
            .map_err(|_| invalid("computer image fence length exceeds bounds"))?;
        check()?;
        out.write_all(MAGIC)?;
        out.write_all(&fence_len.to_be_bytes())?;
        out.write_all(&fence)?;
        check()?;
        self.image.write_validated_to_with_check(out, &mut check)?;
        check()
    }

    /// Read one response for `request`; the callback checks cancellation and
    /// deadline during PNG validation, but cannot interrupt a blocking
    /// `Read`. Callers must use time-bounded IO and discard the stream after
    /// any error because its next packet boundary is then untrusted.
    pub fn read_from_with_check<R, N, C>(
        input: &mut R,
        request: &ComputerFrameRequest,
        mut now_ms: N,
        mut checkpoint: C,
    ) -> io::Result<Self>
    where
        R: Read,
        N: FnMut() -> u64,
        C: FnMut() -> io::Result<()>,
    {
        request_is_valid(request, now_ms())?;
        let mut input = CheckedReader {
            input,
            request,
            now_ms: &mut now_ms,
            checkpoint: &mut checkpoint,
        };
        let mut prefix = [0u8; PREFIX_BYTES];
        input.read_exact(&mut prefix)?;
        if &prefix[..8] != MAGIC {
            return Err(invalid("computer image packet magic is invalid"));
        }
        let fence_len = u32::from_be_bytes(prefix[8..12].try_into().unwrap()) as usize;
        if !(2..=MAX_FENCE_BYTES).contains(&fence_len) {
            return Err(invalid("computer image fence length exceeds bounds"));
        }
        let mut fence_bytes = vec![0; fence_len];
        input.read_exact(&mut fence_bytes)?;
        let fence: ComputerFence = serde_json::from_slice(&fence_bytes)
            .map_err(|_| invalid("computer image fence is malformed"))?;
        fence
            .validate()
            .map_err(|_| invalid("computer image fence is invalid"))?;
        if fence != request.fence {
            return Err(invalid(
                "computer image correlation does not match the request",
            ));
        }
        let image = {
            let CheckedReader {
                input: image_input,
                request: image_request,
                now_ms: image_now_ms,
                checkpoint: image_checkpoint,
            } = &mut input;
            let mut check = || {
                (**image_checkpoint)()?;
                live(image_request, *image_now_ms)
            };
            image::read_from_with_check(image_input, &mut check)?
        };
        if image.meta != request.expected_frame {
            return Err(invalid(
                "computer image metadata does not match the requested frame",
            ));
        }
        input.check()?;
        Ok(Self { fence, image })
    }
}

struct CheckedReader<'a, R, N, C> {
    input: &'a mut R,
    request: &'a ComputerFrameRequest,
    now_ms: &'a mut N,
    checkpoint: &'a mut C,
}

impl<R, N, C> CheckedReader<'_, R, N, C>
where
    R: Read,
    N: FnMut() -> u64,
    C: FnMut() -> io::Result<()>,
{
    fn check(&mut self) -> io::Result<()> {
        (self.checkpoint)()?;
        live(self.request, self.now_ms)
    }
}

impl<R, N, C> Read for CheckedReader<'_, R, N, C>
where
    R: Read,
    N: FnMut() -> u64,
    C: FnMut() -> io::Result<()>,
{
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let read = self.input.read(buffer)?;
        self.check()?;
        Ok(read)
    }

    fn read_exact(&mut self, mut buffer: &mut [u8]) -> io::Result<()> {
        while !buffer.is_empty() {
            // A checkpoint is an admission boundary. Unlike a transient
            // syscall interruption, its Interrupted error must terminate.
            self.check()?;
            let length = match self.input.read(buffer) {
                Ok(0) => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
                Ok(length) => length,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            self.check()?;
            buffer = &mut buffer[length..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use interaction_contracts::desktop::ids::{RequestId, RootTaskId};
    use serde_json::json;
    use sha2::{Digest, Sha256};
    use std::io::Cursor;
    use std::sync::Arc;

    const GOLDEN: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/packet.bin");

    fn image() -> ImageFrame {
        image::read_from(&mut Cursor::new(GOLDEN)).unwrap()
    }

    #[test]
    fn interrupted_checkpoint_after_read_stops_instead_of_retrying_forever() {
        let request = request();
        let mut bytes = Vec::new();
        packet(&request)
            .write_to_with_check(&request, &mut bytes, || 10_000, || Ok(()))
            .unwrap();
        let mut calls = 0;
        let error = ComputerImagePacket::read_from_with_check(
            &mut Cursor::new(bytes),
            &request,
            || 10_000,
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

    fn request() -> ComputerFrameRequest {
        let frame = image().meta;
        ComputerFrameRequest {
            fence: ComputerFence {
                request_id: RequestId::new("computer-image-1").unwrap(),
                generation: 1,
                root_task_id: RootTaskId::new("root-1").unwrap(),
                task_revision: 3,
                root_epoch: 2,
                snapshot_id: "snapshot-8".into(),
                structure_sequence: 8,
                deadline_ms: 20_000,
            },
            expected_frame: frame,
        }
    }

    fn packet(request: &ComputerFrameRequest) -> ComputerImagePacket {
        ComputerImagePacket {
            fence: request.fence.clone(),
            image: image(),
        }
    }

    fn request_for_image(image: &ImageFrame) -> ComputerFrameRequest {
        let mut request = request();
        request.expected_frame = image.meta.clone();
        request
    }

    fn image_with_large_ancillary_chunk() -> ImageFrame {
        let mut frame = image();
        let mut png = frame.bytes.to_vec();
        let mut insert_at = 8;
        while insert_at + 12 <= png.len() {
            let length =
                u32::from_be_bytes(png[insert_at..insert_at + 4].try_into().unwrap()) as usize;
            if &png[insert_at + 4..insert_at + 8] == b"IEND" {
                break;
            }
            insert_at += 12 + length;
        }
        assert!(insert_at + 12 <= png.len(), "fixture has a bounded IEND");

        // A large valid, non-image tEXt chunk makes the SHA loop cross several
        // 64 KiB checkpoints without changing the original pixels.
        let mut text = vec![b'x'; 192 * 1024];
        text[0..2].copy_from_slice(b"k\0");
        let mut chunk = Vec::with_capacity(text.len() + 12);
        chunk.extend_from_slice(&(text.len() as u32).to_be_bytes());
        chunk.extend_from_slice(b"tEXt");
        chunk.extend_from_slice(&text);
        let crc = png_crc32(&chunk[4..]);
        chunk.extend_from_slice(&crc.to_be_bytes());
        png.splice(insert_at..insert_at, chunk);

        frame.meta.byte_length = png.len() as u64;
        frame.meta.checksum = format!("{:x}", Sha256::digest(&png));
        frame.bytes = Arc::from(png);
        frame
    }

    fn png_chunk_count(bytes: &[u8]) -> usize {
        let mut offset = 8;
        let mut count = 0;
        while offset + 12 <= bytes.len() {
            let length = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 12 + length;
            count += 1;
        }
        count
    }

    fn png_crc32(bytes: &[u8]) -> u32 {
        let mut crc = !0u32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            }
        }
        !crc
    }

    #[test]
    fn request_fence_and_exact_complete_image_round_trip() {
        let request = request();
        let packet = packet(&request);
        let mut bytes = Vec::new();
        packet
            .write_to_with_check(&request, &mut bytes, || 10_000, || Ok(()))
            .unwrap();
        assert_eq!(&bytes[..8], MAGIC);
        let decoded = ComputerImagePacket::read_from_with_check(
            &mut Cursor::new(bytes),
            &request,
            || 10_001,
            || Ok(()),
        )
        .unwrap();
        assert_eq!(decoded.fence, request.fence);
        assert_eq!(decoded.image.meta, request.expected_frame);
        assert_eq!(decoded.image.bytes, packet.image.bytes);
    }

    #[test]
    fn checksum_mismatch_is_rejected_on_write_and_read() {
        let request = request();
        let mut damaged_packet = packet(&request);
        let mut damaged = damaged_packet.image.bytes.to_vec();
        let damaged_index = damaged.len() - 9;
        damaged[damaged_index] ^= 1;
        damaged_packet.image.bytes = Arc::from(damaged);
        let mut output = Vec::new();
        assert!(
            damaged_packet
                .write_to_with_check(&request, &mut output, || 10_000, || Ok(()))
                .is_err()
        );
        assert!(
            output.is_empty(),
            "invalid pixels must fail before the fence is written"
        );

        let good = packet(&request);
        let mut output = Vec::new();
        good.write_to_with_check(&request, &mut output, || 10_000, || Ok(()))
            .unwrap();
        let image_start =
            PREFIX_BYTES + u32::from_be_bytes(output[8..12].try_into().unwrap()) as usize;
        let metadata_len = u32::from_be_bytes(
            output[image_start + 8..image_start + 12]
                .try_into()
                .unwrap(),
        ) as usize;
        let png_start = image_start + 16 + metadata_len;
        output[png_start + 40] ^= 1;
        assert!(
            ComputerImagePacket::read_from_with_check(
                &mut Cursor::new(output),
                &request,
                || 10_001,
                || Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn cancelled_or_expired_during_sha_validation_writes_no_outer_prefix() {
        let image = image_with_large_ancillary_chunk();
        let request = request_for_image(&image);
        let packet = ComputerImagePacket {
            fence: request.fence.clone(),
            image,
        };
        let sha_cancel_call = 1 + png_chunk_count(&packet.image.bytes) + 3;

        let mut output = Vec::new();
        let mut calls = 0;
        let error = packet
            .write_to_with_check(
                &request,
                &mut output,
                || 10_000,
                || {
                    calls += 1;
                    if calls == sha_cancel_call {
                        Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                    } else {
                        Ok(())
                    }
                },
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(
            output.is_empty(),
            "validation must finish before any prefix"
        );

        let deadline_call = 1 + sha_cancel_call;
        let clock_calls = std::cell::Cell::new(0usize);
        let mut output = Vec::new();
        let error = packet
            .write_to_with_check(
                &request,
                &mut output,
                || {
                    let calls = clock_calls.get() + 1;
                    clock_calls.set(calls);
                    if calls >= deadline_call {
                        request.fence.deadline_ms
                    } else {
                        10_000
                    }
                },
                || Ok(()),
            )
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(output.is_empty(), "expired validation must write no prefix");
    }

    #[test]
    fn reader_checks_cancellation_during_png_sha_validation() {
        let image = image_with_large_ancillary_chunk();
        let request = request_for_image(&image);
        let packet = ComputerImagePacket {
            fence: request.fence.clone(),
            image,
        };
        let mut bytes = Vec::new();
        packet
            .write_to_with_check(&request, &mut bytes, || 10_000, || Ok(()))
            .unwrap();

        // Four outer-header/fence checks, six nested ImageFrame read checks,
        // then cancel on the third SHA chunk after two chunks were hashed.
        let sha_cancel_call = 10 + 1 + png_chunk_count(&packet.image.bytes) + 3;
        let mut calls = 0;
        let error = ComputerImagePacket::read_from_with_check(
            &mut Cursor::new(bytes),
            &request,
            || 10_001,
            || {
                calls += 1;
                if calls == sha_cancel_call {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    }

    #[test]
    fn exact_expected_frame_metadata_and_request_correlation_are_required() {
        let request = request();
        let mut mismatch = request.clone();
        mismatch.expected_frame.source_sequence += 1;
        assert!(
            packet(&request)
                .write_to_with_check(&mismatch, &mut Vec::new(), || 10_000, || Ok(()))
                .is_err()
        );

        let mut bytes = Vec::new();
        packet(&request)
            .write_to_with_check(&request, &mut bytes, || 10_000, || Ok(()))
            .unwrap();
        let mut foreign = request.clone();
        foreign.fence.request_id = RequestId::new("computer-image-2").unwrap();
        assert!(
            ComputerImagePacket::read_from_with_check(
                &mut Cursor::new(bytes),
                &foreign,
                || 10_001,
                || Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn expired_requests_oversized_headers_and_truncated_packets_fail_closed() {
        let request = request();
        assert!(
            packet(&request)
                .write_to_with_check(
                    &request,
                    &mut Vec::new(),
                    || request.fence.deadline_ms,
                    || Ok(())
                )
                .is_err()
        );

        let mut oversized = Vec::from(MAGIC.as_slice());
        oversized.extend_from_slice(&((MAX_FENCE_BYTES as u32) + 1).to_be_bytes());
        assert!(
            ComputerImagePacket::read_from_with_check(
                &mut Cursor::new(oversized),
                &request,
                || 10_000,
                || Ok(()),
            )
            .is_err()
        );

        let mut bytes = Vec::new();
        packet(&request)
            .write_to_with_check(&request, &mut bytes, || 10_000, || Ok(()))
            .unwrap();
        bytes.pop();
        assert!(
            ComputerImagePacket::read_from_with_check(
                &mut Cursor::new(bytes),
                &request,
                || 10_001,
                || Ok(()),
            )
            .is_err()
        );
    }

    #[test]
    fn invalid_fence_json_is_not_accepted() {
        let mut bytes = Vec::from(MAGIC.as_slice());
        let fence = serde_json::to_vec(&json!({"request_id":"foreign","unknown":true})).unwrap();
        bytes.extend_from_slice(&(fence.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&fence);
        assert!(
            ComputerImagePacket::read_from_with_check(
                &mut Cursor::new(bytes),
                &request(),
                || 10_000,
                || Ok(()),
            )
            .is_err()
        );
    }
}
