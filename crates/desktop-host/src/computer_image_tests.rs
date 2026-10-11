use super::*;
use desktop_ipc::computer_control::{
    ComputerImageRejection, ComputerImageRejectionReason, ComputerPortRequest, ComputerSafetyCheck,
    read_port_request, read_request, write_rejection, write_safety_reply,
};
use desktop_ipc::computer_image::ComputerImagePacket;
use desktop_ipc::image::ImageFrame;
use std::io::{Read, Write};
use std::sync::atomic::AtomicU64;
use std::time::Instant;

const PNG: &[u8] = include_bytes!("../../../contracts/desktop-v1/image-channel/fixture.png");

fn request() -> ComputerFrameRequest {
    serde_json::from_str(include_str!(
        "../../../contracts/computer-v1/image-channel/request.json"
    ))
    .unwrap()
}

fn port() -> (ComputerImagePort, UnixStream, Arc<AtomicU64>) {
    let (host, main) = UnixStream::pair().unwrap();
    main.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    main.set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let now = Arc::new(AtomicU64::new(40));
    let clock = now.clone();
    (
        ComputerImagePort::new(host, Arc::new(move || clock.load(Ordering::SeqCst))).unwrap(),
        main,
        now,
    )
}

fn poll(port: &mut ComputerImagePort) -> ComputerImagePortResult {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(ComputerPortResult::Image(result)) = port.poll() {
            let mut result = *result;
            if result.cancelled && result.response.is_ok() {
                result.response = Err("retired computer scope".into());
            }
            return result;
        }
        assert!(Instant::now() < deadline, "image worker did not close");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn poll_safety(port: &mut ComputerImagePort) -> ComputerSafetyPortResult {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(ComputerPortResult::Safety(result)) = port.poll() {
            return *result;
        }
        assert!(Instant::now() < deadline, "safety worker did not close");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn safety() -> ComputerSafetyRequest {
    ComputerSafetyRequest {
        fence: request().fence,
        pending_safety_checks: vec![ComputerSafetyCheck {
            id: "check-1".into(),
            code: Some("confirm".into()),
            message: Some("Review the action".into()),
        }],
        proposal: None,
    }
}

#[test]
fn a_cancelled_confirmation_is_consumed_before_the_next_root_uses_the_port() {
    let (mut host, mut main, _) = port();
    let first = safety();
    let mut next = first.clone();
    next.fence.generation += 1;
    next.fence.request_id =
        interaction_contracts::desktop::ids::RequestId::new("next-safety-request").unwrap();
    next.fence.root_task_id =
        interaction_contracts::desktop::ids::RootTaskId::new("next-root").unwrap();
    let (ready, waiting) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let ComputerPortRequest::Safety(first) = read_port_request(&mut main, || Ok(())).unwrap()
        else {
            panic!("expected safety");
        };
        ready.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(2)).unwrap();
        write_safety_reply(
            &ComputerSafetyReply {
                fence: first.fence,
                approved: true,
            },
            &mut main,
            || Ok(()),
        )
        .unwrap();
        let ComputerPortRequest::Safety(next) = read_port_request(&mut main, || Ok(())).unwrap()
        else {
            panic!("expected safety");
        };
        write_safety_reply(
            &ComputerSafetyReply {
                fence: next.fence,
                approved: false,
            },
            &mut main,
            || Ok(()),
        )
        .unwrap();
    });
    host.request_safety(first.clone()).unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    host.cancel_scope(&first.fence);
    assert!(host.request_safety(next.clone()).is_err());
    release.send(()).unwrap();
    let retired = poll_safety(&mut host);
    assert!(retired.cancelled);
    assert!(retired.response.unwrap().approved);
    assert!(!host.is_closed());
    host.request_safety(next.clone()).unwrap();
    let current = poll_safety(&mut host);
    assert!(!current.cancelled);
    assert_eq!(current.request, next);
    assert!(!current.response.unwrap().approved);
    server.join().unwrap();
}

#[test]
fn stop_interrupts_a_pending_safety_dialog_without_waiting_for_its_reply() {
    let (mut host, mut main, _) = port();
    let (ready, waiting) = mpsc::channel();
    let server = std::thread::spawn(move || {
        read_port_request(&mut main, || Ok(())).unwrap();
        ready.send(()).unwrap();
        let mut byte = [0];
        assert_eq!(main.read(&mut byte).unwrap(), 0);
    });
    host.request_safety(safety()).unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    let before = Instant::now();
    host.close();
    assert!(before.elapsed() < Duration::from_millis(150));
    let cancelled = poll_safety(&mut host);
    assert!(cancelled.cancelled);
    assert!(cancelled.response.is_err());
    server.join().unwrap();
}

#[test]
fn original_frame_can_arrive_after_the_main_encoder_warms_up() {
    let (mut host, mut main, _) = port();
    let expected = request();
    let original = expected.clone();
    let server = std::thread::spawn(move || {
        let request = read_request(&mut main, || Ok(())).unwrap();
        assert_eq!(request, original);
        std::thread::sleep(Duration::from_millis(300));
        ComputerImagePacket {
            fence: request.fence.clone(),
            image: ImageFrame {
                meta: request.expected_frame.clone(),
                bytes: Arc::from(PNG),
            },
        }
        .write_to_with_check(&request, &mut main, || 40, || Ok(()))
        .unwrap();
    });
    host.request(expected.clone()).unwrap();
    assert!(host.request(expected.clone()).is_err());
    let result = poll(&mut host);
    assert_eq!(result.request, expected);
    let ComputerImageResponse::Image(packet) = result.response.unwrap() else {
        panic!("expected original image")
    };
    assert_eq!(packet.image.bytes.as_ref(), PNG);
    assert_eq!(packet.image.meta, expected.expected_frame);
    assert!(host.poll().is_none());
    server.join().unwrap();
}

#[test]
fn stop_interrupts_a_partial_reply_and_permanently_seals_the_port() {
    let (mut host, mut main, _) = port();
    let (ready, waiting) = mpsc::channel();
    let server = std::thread::spawn(move || {
        read_request(&mut main, || Ok(())).unwrap();
        main.write_all(b"CUC").unwrap();
        ready.send(()).unwrap();
        let mut one = [0u8; 1];
        assert_eq!(main.read(&mut one).unwrap(), 0);
    });
    host.request(request()).unwrap();
    waiting.recv_timeout(Duration::from_secs(2)).unwrap();
    let before = Instant::now();
    host.close();
    assert!(before.elapsed() < Duration::from_millis(150));
    assert!(poll(&mut host).response.is_err());
    assert!(host.request(request()).is_err());
    server.join().unwrap();
}

#[test]
fn foreign_rejection_is_a_protocol_fault_and_cannot_be_reused() {
    let (mut host, mut main, _) = port();
    let server = std::thread::spawn(move || {
        let mut request = read_request(&mut main, || Ok(())).unwrap();
        request.fence.generation += 1;
        write_rejection(
            &ComputerImageRejection {
                fence: request.fence,
                reason: ComputerImageRejectionReason::FrameUnavailable,
            },
            &mut main,
            || Ok(()),
        )
        .unwrap();
    });
    host.request(request()).unwrap();
    assert!(poll(&mut host).response.is_err());
    assert!(host.request(request()).is_err());
    server.join().unwrap();
}

#[test]
fn correlated_rejection_does_not_fabricate_pixels_or_close_the_protocol() {
    let (mut host, mut main, _) = port();
    let server = std::thread::spawn(move || {
        for _ in 0..2 {
            let request = read_request(&mut main, || Ok(())).unwrap();
            write_rejection(
                &ComputerImageRejection {
                    fence: request.fence,
                    reason: ComputerImageRejectionReason::FrameUnavailable,
                },
                &mut main,
                || Ok(()),
            )
            .unwrap();
        }
    });
    for _ in 0..2 {
        host.request(request()).unwrap();
        assert!(matches!(
            poll(&mut host).response,
            Ok(ComputerImageResponse::Rejected(_))
        ));
    }
    server.join().unwrap();
}

#[test]
fn queued_image_is_rejected_when_the_host_deadline_passes() {
    let (mut host, mut main, now) = port();
    let (sent, ready) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let request = read_request(&mut main, || Ok(())).unwrap();
        ComputerImagePacket {
            fence: request.fence.clone(),
            image: ImageFrame {
                meta: request.expected_frame.clone(),
                bytes: Arc::from(PNG),
            },
        }
        .write_to_with_check(&request, &mut main, || 40, || Ok(()))
        .unwrap();
        sent.send(()).unwrap();
    });
    host.request(request()).unwrap();
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    now.store(20_000, Ordering::SeqCst);
    assert!(poll(&mut host).response.is_err());
    server.join().unwrap();
}

#[test]
fn a_partial_reply_cannot_restart_the_original_io_deadline() {
    let (socket, mut main) = UnixStream::pair().unwrap();
    main.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let started = Instant::now();
    let clock = Arc::new(move || 40 + u64::try_from(started.elapsed().as_millis()).unwrap());
    let mut host = ComputerImagePort::new(socket, clock).unwrap();
    let mut request = request();
    request.fence.deadline_ms = 1_040;
    let server = std::thread::spawn(move || {
        read_request(&mut main, || Ok(())).unwrap();
        std::thread::sleep(Duration::from_millis(800));
        main.write_all(b"CUCIREJ1").unwrap();
        let mut buffer = [0; 1];
        assert_eq!(main.read(&mut buffer).unwrap(), 0);
    });
    host.request(request).unwrap();
    assert!(poll(&mut host).response.is_err());
    assert!(started.elapsed() < Duration::from_millis(1_500));
    server.join().unwrap();
}
