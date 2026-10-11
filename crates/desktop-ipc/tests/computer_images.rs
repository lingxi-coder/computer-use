use std::io::Cursor;

use desktop_ipc::computer_image::ComputerImagePacket;
use interaction_contracts::desktop::computer::ComputerFrameRequest;

const PACKET: &[u8] = include_bytes!("../../../contracts/computer-v1/image-channel/packet.bin");
const REQUEST: &[u8] = include_bytes!("../../../contracts/computer-v1/image-channel/request.json");

#[test]
fn typescript_computer_packet_decodes_and_rust_reencoding_preserves_the_image_and_fence() {
    let request: ComputerFrameRequest = serde_json::from_slice(REQUEST).unwrap();
    let packet = ComputerImagePacket::read_from_with_check(
        &mut Cursor::new(PACKET),
        &request,
        || 10_000,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(packet.fence, request.fence);
    assert_eq!(packet.image.meta, request.expected_frame);
    assert_eq!(
        packet.image.bytes.as_ref(),
        include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png")
    );
    let mut encoded = Vec::new();
    packet
        .write_to_with_check(&request, &mut encoded, || 10_001, || Ok(()))
        .unwrap();
    let roundtrip = ComputerImagePacket::read_from_with_check(
        &mut Cursor::new(encoded),
        &request,
        || 10_002,
        || Ok(()),
    )
    .unwrap();
    assert_eq!(roundtrip.fence, packet.fence);
    assert_eq!(roundtrip.image.meta, packet.image.meta);
    assert_eq!(roundtrip.image.bytes, packet.image.bytes);
}
