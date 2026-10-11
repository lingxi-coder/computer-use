use desktop_ipc::image::{self, ImageFrame};
use interaction_contracts::desktop::source::FrameMeta;
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::sync::Arc;
const GOLDEN: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/packet.bin");
const PNG: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png");
const META: &str = include_str!("../../../contracts/desktop-v1/image-channel/metadata.json");

#[test]
fn shared_packet_round_trips_without_rewriting_pixel_provenance() {
    let frame = image::read_from(&mut Cursor::new(GOLDEN)).unwrap();
    let meta: FrameMeta = serde_json::from_str(META).unwrap();
    assert_eq!(frame.meta, meta);
    assert_eq!(&*frame.bytes, PNG);
    let mut bytes = Vec::new();
    frame.write_to(&mut bytes).unwrap();
    let again = image::read_from(&mut Cursor::new(bytes)).unwrap();
    assert_eq!(again.meta.source_ms, 10);
    assert_eq!(again.meta.checksum, meta.checksum);
}
#[test]
fn forged_lengths_and_truncated_packets_fail_before_unbounded_allocation() {
    for field in [8, 12] {
        let mut bytes = GOLDEN[..16].to_vec();
        bytes[field..field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(image::read_from(&mut Cursor::new(bytes)).is_err());
    }
    for length in [0, 15, 100, GOLDEN.len() - 1] {
        assert!(image::read_from(&mut Cursor::new(&GOLDEN[..length])).is_err());
    }
}
#[test]
fn checksum_dimensions_and_animation_cannot_be_accepted_as_a_single_frame() {
    let mut frame = image::read_from(&mut Cursor::new(GOLDEN)).unwrap();
    let mut corrupt = PNG.to_vec();
    corrupt[45] ^= 1;
    assert!(ImageFrame::new(frame.meta.clone(), corrupt.into()).is_err());
    frame.meta.width_px += 1;
    assert!(ImageFrame::new(frame.meta.clone(), Arc::from(PNG)).is_err());
    frame.meta.width_px -= 1;
    let mut animated = PNG[..33].to_vec();
    animated.extend_from_slice(&[0, 0, 0, 0, b'a', b'c', b'T', b'L', 0, 0, 0, 0]);
    animated.extend_from_slice(&PNG[33..]);
    frame.meta.byte_length = animated.len() as u64;
    frame.meta.checksum = format!("{:x}", Sha256::digest(&animated));
    assert!(ImageFrame::new(frame.meta, animated.into()).is_err());
}

#[test]
fn cancelling_large_checksum_work_publishes_no_packet_prefix() {
    let mut frame = image::read_from(&mut Cursor::new(GOLDEN)).unwrap();
    let mut bytes = PNG[..33].to_vec();
    let padding = 8 * 1024 * 1024;
    bytes.extend_from_slice(&(padding as u32).to_be_bytes());
    bytes.extend_from_slice(b"tEXt");
    bytes.resize(bytes.len() + padding + 4, 0);
    bytes.extend_from_slice(&PNG[33..]);
    frame.meta.byte_length = bytes.len() as u64;
    frame.meta.checksum = format!("{:x}", Sha256::digest(&bytes));
    frame.bytes = bytes.into();
    let mut output = Vec::new();
    let mut checks = 0;
    let error = frame
        .write_to_with_check(&mut output, || {
            checks += 1;
            if checks >= 10 {
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "cancelled while validating",
                ))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        output.is_empty(),
        "a cancelled checksum must not publish a partial packet"
    );
}
