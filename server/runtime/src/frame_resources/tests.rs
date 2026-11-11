use super::*;
use chr_common::scene_frame::{Alpha, TimeBase, Transfer, U64};

fn id(value: &str) -> Id {
    Id::try_from(value.to_owned()).unwrap()
}

fn image() -> ImageDescription {
    ImageDescription {
        extent: Extent {
            width: 1,
            height: 1,
        },
        format: ImageFormat::Rgba8Unorm,
        transfer: Transfer::Srgb,
        alpha: Alpha::Straight,
    }
}

fn metadata(sequence: u64) -> FrameMetadata {
    FrameMetadata {
        frame: FrameKey {
            stream_id: id("synthetic"),
            sequence: U64::from(sequence),
        },
        timeline: Timeline {
            timeline_id: id("timeline"),
            time_base: TimeBase::new(1, 50).unwrap(),
        },
        position: Position {
            timeline_id: id("timeline"),
            ticks: U64::from(sequence),
        },
        image: image(),
    }
}

fn owner(capacity: usize, bytes: usize) -> FrameResources {
    FrameResources::new(
        ResourceConfig {
            runtime_id: id("runtime"),
            native_extent: image().extent,
            max_frames: capacity,
            max_frame_bytes: bytes,
            max_assets: 1,
            max_asset_bytes: 4,
            max_producers: 1,
        },
        vec![id("synthetic")],
    )
    .unwrap()
}

fn register(
    owner: &mut FrameResources,
    sequence: u64,
) -> (WriteTicket, ProducerGuard<FrameMetadata>) {
    let producer = owner.producer(&id("synthetic")).unwrap();
    owner
        .register(&producer, metadata(sequence), &[1, 2, 3, 255])
        .unwrap()
}

#[test]
fn publication_is_one_shot_and_metadata_failure_does_not_consume_ticket() {
    let mut owner = owner(1, 4);
    let (ticket, guard) = register(&mut owner, 91);
    assert!(matches!(
        owner.publish(&ticket, &metadata(92)),
        Err(ResourceError::IncompatibleInput)
    ));
    let image = owner.publish(&ticket, &metadata(91)).unwrap();
    assert!(matches!(
        owner.publish(&ticket, &metadata(91)),
        Err(ResourceError::InvalidState)
    ));
    assert!(!owner.cancel_write(&ticket).unwrap());
    let lease = owner.acquire(&image).unwrap();
    assert_eq!(lease.metadata().frame.sequence.get(), 91);
    assert_eq!(lease.pixels(), Err(ResourceError::WritePending));
    assert!(matches!(
        lease.begin_work(),
        Err(ResourceError::WritePending)
    ));
    guard.ready().unwrap();
    assert_eq!(lease.pixels().unwrap(), &[1, 2, 3, 255]);
    assert_eq!(guard.fail(), Err(ResourceError::InvalidState));
    guard.complete();
}

#[test]
fn release_and_cancel_do_not_complete_producer_or_recycle_slot() {
    let mut owner = owner(1, 4);
    let (ticket, guard) = register(&mut owner, 8);
    let image = owner.publish(&ticket, &metadata(8)).unwrap();
    assert!(owner.release(&image).unwrap());
    assert!(!owner.release(&image).unwrap());
    assert!(matches!(
        owner.acquire(&image),
        Err(ResourceError::NotFound)
    ));
    let producer = owner.producer(&id("synthetic")).unwrap();
    assert!(matches!(
        owner.register(&producer, metadata(3), &[0; 4]),
        Err(ResourceError::Capacity)
    ));
    guard.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
    let (replacement, guard) = register(&mut owner, 3);
    assert_eq!(ticket.key.slot, replacement.key.slot);
    assert_ne!(ticket.key.generation, replacement.key.generation);
    assert!(matches!(
        owner.publish(&ticket, &metadata(8)),
        Err(ResourceError::NotFound)
    ));
    assert!(owner.cancel_write(&replacement).unwrap());
    assert!(!owner.cancel_write(&replacement).unwrap());
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 1);
    guard.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
}

#[test]
fn failure_is_visible_before_physical_completion_and_cleanup() {
    let mut owner = owner(1, 4);
    let (ticket, producer) = register(&mut owner, 0);
    let image = owner.publish(&ticket, &metadata(0)).unwrap();
    let lease = owner.acquire(&image).unwrap();
    producer.fail().unwrap();
    assert_eq!(lease.readiness(), Readiness::Failed);
    assert_eq!(lease.pixels(), Err(ResourceError::WriteFailed));
    assert!(owner.release(&image).unwrap());
    drop(lease);
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 1);
    producer.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
}

#[test]
fn accepted_pending_lease_survives_revocation_and_producer_completion() {
    let mut owner = owner(1, 4);
    let (ticket, producer) = register(&mut owner, 0);
    let image = owner.publish(&ticket, &metadata(0)).unwrap();
    let accepted = owner.acquire(&image).unwrap();
    owner.release(&image).unwrap();
    assert_eq!(accepted.pixels(), Err(ResourceError::WritePending));
    producer.ready().unwrap();
    assert_eq!(accepted.pixels().unwrap(), &[1, 2, 3, 255]);
    producer.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 1);
    drop(accepted);
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
}

#[test]
fn concurrent_consumers_keep_input_through_release_and_gpu_completion() {
    let mut owner = owner(1, 4);
    let (ticket, producer) = register(&mut owner, 0);
    let image = owner.publish(&ticket, &metadata(0)).unwrap();
    let first = owner.acquire(&image).unwrap();
    let second = owner.acquire(&image).unwrap();
    producer.ready().unwrap();
    producer.complete();
    let work = first.begin_work().unwrap();
    drop(first);
    owner.release(&image).unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        assert_eq!(second.pixels().unwrap(), &[1, 2, 3, 255]);
        done_tx.send(()).unwrap();
        release_rx.recv().unwrap();
        drop(second);
    });
    done_rx.recv().unwrap();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 1);
    assert_eq!(work.pixels(), &[1, 2, 3, 255]);
    work.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 1);
    release_tx.send(()).unwrap();
    worker.join().unwrap();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
}

#[test]
fn dropped_completion_observers_fail_closed_with_no_overflow_pool() {
    let mut owner = owner(2, 8);
    let (ticket, producer) = register(&mut owner, 0);
    let image = owner.publish(&ticket, &metadata(0)).unwrap();
    let lease = owner.acquire(&image).unwrap();
    drop(producer);
    assert_eq!(lease.readiness(), Readiness::Failed);
    drop(lease);
    owner.release(&image).unwrap();
    let (ticket, producer) = register(&mut owner, 1);
    let image = owner.publish(&ticket, &metadata(1)).unwrap();
    producer.ready().unwrap();
    producer.complete();
    let lease = owner.acquire(&image).unwrap();
    let work = lease.begin_work().unwrap();
    drop(work);
    drop(lease);
    owner.release(&image).unwrap();
    let producer = owner.producer(&id("synthetic")).unwrap();
    for sequence in 2..102 {
        assert!(matches!(
            owner.register(&producer, metadata(sequence), &[0; 4]),
            Err(ResourceError::Capacity)
        ));
        assert_eq!(owner.occupied_frames(), 2);
    }
}

#[test]
fn foreign_unknown_and_generation_mismatch_are_rejected() {
    let mut first = owner(1, 4);
    let mut other = owner(1, 4);
    let (ticket, producer) = register(&mut first, 0);
    assert!(matches!(
        other.publish(&ticket, &metadata(0)),
        Err(ResourceError::NotFound)
    ));
    let image = first.publish(&ticket, &metadata(0)).unwrap();
    assert!(matches!(
        other.acquire(&image),
        Err(ResourceError::NotFound)
    ));
    let unknown = ImageRef {
        owner: Arc::clone(&first.identity),
        key: Key {
            slot: usize::MAX,
            generation: 1,
        },
    };
    assert!(matches!(
        first.acquire(&unknown),
        Err(ResourceError::NotFound)
    ));
    let stale = ImageRef {
        owner: Arc::clone(&first.identity),
        key: Key {
            slot: image.key.slot,
            generation: image.key.generation + 1,
        },
    };
    assert!(matches!(
        first.acquire(&stale),
        Err(ResourceError::NotFound)
    ));
    assert!(matches!(
        first.producer(&id("unregistered")),
        Err(ResourceError::NotFound)
    ));
    producer.complete();
}

#[test]
fn byte_budget_and_bounded_reclamation_cover_out_of_order_frames() {
    let mut owner = owner(3, 8);
    let producer = owner.producer(&id("synthetic")).unwrap();
    for _ in 0..32 {
        let (a, ga) = register(&mut owner, 300);
        let (b, gb) = register(&mut owner, 2);
        assert!(matches!(
            owner.register(&producer, metadata(99), &[0; 4]),
            Err(ResourceError::Capacity)
        ));
        let a = owner.publish(&a, &metadata(300)).unwrap();
        let b = owner.publish(&b, &metadata(2)).unwrap();
        ga.ready().unwrap();
        gb.ready().unwrap();
        ga.complete();
        gb.complete();
        assert_eq!(
            owner.acquire(&a).unwrap().metadata().frame.sequence.get(),
            300
        );
        assert_eq!(
            owner.acquire(&b).unwrap().metadata().frame.sequence.get(),
            2
        );
        owner.release(&b).unwrap();
        owner.release(&a).unwrap();
        assert_eq!(owner.occupied_frames(), 0);
    }
}

#[test]
fn invalid_registration_does_not_spend_capacity_and_assets_have_separate_budget() {
    let mut owner = owner(1, 4);
    let producer = owner.producer(&id("synthetic")).unwrap();
    let mut wrong = metadata(0);
    wrong.position.timeline_id = id("other");
    assert!(matches!(
        owner.register(&producer, wrong, &[0; 4]),
        Err(ResourceError::IncompatibleInput)
    ));
    assert!(matches!(
        owner.register(&producer, metadata(0), &[0; 3]),
        Err(ResourceError::InvalidPixels)
    ));
    let mut float = image();
    float.format = ImageFormat::Rgba16Float;
    float.transfer = Transfer::Linear;
    assert!(matches!(
        owner.register_asset(float.clone(), &[0, 0x7c, 0, 0, 0, 0, 0, 0]),
        Err(ResourceError::InvalidPixels)
    ));
    float.transfer = Transfer::Srgb;
    assert!(matches!(
        owner.register_asset(float, &[0; 8]),
        Err(ResourceError::InvalidPixels)
    ));
    let asset = owner.register_asset(image(), &[4, 3, 2, 1]).unwrap();
    let retained = owner.acquire_asset(&asset).unwrap();
    let (ticket, guard) = register(&mut owner, 0);
    assert_eq!(owner.occupied_assets(), 1);
    assert_eq!(owner.occupied_frames(), 1);
    owner.release_asset(&asset).unwrap();
    assert!(matches!(
        owner.register_asset(image(), &[0; 4]),
        Err(ResourceError::Capacity)
    ));
    assert_eq!(retained.pixels().unwrap(), &[4, 3, 2, 1]);
    drop(retained);
    owner.reclaim();
    assert_eq!(owner.occupied_assets(), 0);
    owner.cancel_write(&ticket).unwrap();
    guard.complete();
    owner.reclaim();
    assert_eq!(owner.occupied_frames(), 0);
}

#[test]
fn registration_copies_content_and_requires_its_registered_stream_and_extent() {
    let mut owner = owner(1, 4);
    let producer = owner.producer(&id("synthetic")).unwrap();
    let mut invalid = metadata(0);
    invalid.frame.stream_id = id("another-stream");
    assert!(matches!(
        owner.register(&producer, invalid, &[0; 4]),
        Err(ResourceError::IncompatibleInput)
    ));
    let mut invalid = metadata(0);
    invalid.image.extent.width = 2;
    assert!(matches!(
        owner.register(&producer, invalid, &[0; 8]),
        Err(ResourceError::IncompatibleInput)
    ));
    let mut source = [5, 6, 7, 255];
    let (ticket, guard) = owner.register(&producer, metadata(0), &source).unwrap();
    source.fill(0);
    let image = owner.publish(&ticket, &metadata(0)).unwrap();
    guard.ready().unwrap();
    guard.complete();
    assert_eq!(
        owner.acquire(&image).unwrap().pixels().unwrap(),
        &[5, 6, 7, 255]
    );
}
