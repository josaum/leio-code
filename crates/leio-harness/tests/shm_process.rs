use leio_harness::shm::ShmRingBuffer;
use std::{
    process::Command,
    time::{Duration, Instant},
};

#[test]
fn shm_child() {
    let Ok(name) = std::env::var("LEIO_TEST_SHM") else {
        return;
    };
    let mut consumer = ShmRingBuffer::attach(&name, 4).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    for expected in 1..=2000u64 {
        loop {
            if let Some(slot) = consumer.pop() {
                assert_eq!(slot.seq, expected);
                assert_eq!(slot.vector_slice(), &[expected as f32, -(expected as f32)]);
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
}

#[test]
fn cross_process_wraparound_preserves_every_sequence_and_payload() {
    let name = format!("leio_xproc_{}", std::process::id());
    let mut producer = ShmRingBuffer::create(&name, 4).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "shm_child", "--nocapture"])
        .env("LEIO_TEST_SHM", &name)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    for sequence in 1..=2000u64 {
        loop {
            if producer
                .push(1, "producer", "run", &[sequence as f32, -(sequence as f32)])
                .is_ok()
            {
                break;
            }
            assert!(Instant::now() < deadline);
            std::thread::yield_now();
        }
    }
    assert!(child.wait().unwrap().success());
}

#[test]
fn attach_never_creates_resizes_or_reinitializes_segments() {
    let name = format!("leio_attach_{}", std::process::id());
    assert!(ShmRingBuffer::attach(&name, 4).is_err());
    let mut owner = ShmRingBuffer::create(&name, 4).unwrap();
    owner.push(1, "a", "r", &[42.]).unwrap();
    assert!(ShmRingBuffer::create(&name, 4).is_err());
    assert!(ShmRingBuffer::attach(&name, 2).is_err());
    assert!(ShmRingBuffer::attach(&name, 8).is_err());
    assert!(ShmRingBuffer::attach(&name, 0).is_err());
    assert_eq!(owner.pop().unwrap().vector_slice(), &[42.]);
    assert!(ShmRingBuffer::create("abcdefghijklmnopqrstuvwxyz123456789", 4).is_err());
}

#[test]
fn claims_are_handle_owned_and_snapshot_survives_slot_reuse() {
    let name = format!("leio_claim_{}", std::process::id());
    let mut owner = ShmRingBuffer::create(&name, 1).unwrap();
    let mut verifier = ShmRingBuffer::attach(&name, 1).unwrap();
    let mut intruder = ShmRingBuffer::attach(&name, 1).unwrap();
    let (_, index) = owner.push_tentative(1, 1, 1., "a", "r", &[42.]).unwrap();
    assert!(verifier.try_claim_verification(index, "verifier"));
    assert!(intruder.commit_verification(index, "verifier").is_err());
    assert!(owner.pop_any().is_none());
    verifier.commit_verification(index, "verifier").unwrap();
    let snapshot = owner.peek_vector().unwrap();
    assert_eq!(owner.pop().unwrap().vector_slice(), &[42.]);
    assert!(owner.peek_slot(index).is_none());
    owner.push(1, "a", "r", &[99.]).unwrap();
    assert_eq!(snapshot, vec![42.]);
    assert!(verifier.abort_verification(index, "verifier", 1).is_err());
    assert_eq!(owner.pop().unwrap().vector_slice(), &[99.]);
}
