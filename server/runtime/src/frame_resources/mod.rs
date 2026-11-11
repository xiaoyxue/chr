//! Bounded native input ownership for a trusted, explicitly configured synthetic
//! controller. No RPC registration, capture, renderer scheduling, or output port.
//!
//! Synthetic bytes are copied at registration; readiness gates their visibility.
//! Opaque native capabilities are owner-scoped and never encode a slot on wire.
//! Opaque common IDs resolve within the same bounded cells; adapters do not need
//! another ownership registry or arbitrary filesystem paths.

mod core;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use chr_common::scene_frame::{
    Extent, FrameKey, Id, ImageDescription, ImageFormat, Position, Timeline, Validate,
};

use core::{Key, Pool};

static NEXT_OWNER: AtomicU64 = AtomicU64::new(0);
pub use core::{Lease, ProducerGuard, Readiness, WorkGuard};

/// Native failures; mapping to shared wire rejection codes belongs to the adapter.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ResourceError {
    #[error("invalid explicit resource configuration")]
    InvalidConfiguration,
    #[error("unknown, revoked, stale, or foreign native capability")]
    NotFound,
    #[error("resource metadata does not match its registration")]
    IncompatibleInput,
    #[error("resource operation is not valid in its current state")]
    InvalidState,
    #[error("bounded resource capacity exhausted")]
    Capacity,
    #[error("producer write is not ready")]
    WritePending,
    #[error("producer write failed")]
    WriteFailed,
    #[error("unsupported or invalid synthetic pixels")]
    InvalidPixels,
}

/// Immutable registered source identity. Frame sequence is unrelated to storage,
/// arrival order, submission IDs, runtime effects, or output slots.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameMetadata {
    pub frame: FrameKey,
    pub timeline: Timeline,
    pub position: Position,
    pub image: ImageDescription,
}

/// All resource limits are explicit. This is not the full render configuration;
/// output color/timing/quality installation remains a separate runtime boundary.
pub struct ResourceConfig {
    pub runtime_id: Id,
    pub native_extent: Extent,
    pub max_frames: usize,
    pub max_frame_bytes: usize,
    pub max_assets: usize,
    pub max_asset_bytes: usize,
    pub max_producers: usize,
}

/// In-process producer authorization. Only configured streams can receive one.
#[derive(Clone)]
pub struct Producer {
    owner: Arc<()>,
    stream: Id,
}

/// One-use publication capability. Cloning does not grant another publication.
#[derive(Clone)]
pub struct WriteTicket {
    owner: Arc<()>,
    key: Key,
    id: Id,
}

impl WriteTicket {
    pub fn id(&self) -> &Id {
        &self.id
    }
}

/// Opaque public input reference; not an output resource or a storage address.
#[derive(Clone)]
pub struct ImageRef {
    owner: Arc<()>,
    key: Key,
    id: Id,
}

impl ImageRef {
    pub fn id(&self) -> &Id {
        &self.id
    }
}

/// Immutable synthetic image asset, separate from frame and output namespaces.
#[derive(Clone)]
pub struct AssetRef {
    owner: Arc<()>,
    key: Key,
    id: Id,
}

impl AssetRef {
    pub fn id(&self) -> &Id {
        &self.id
    }
}

/// Single-controller owner with concurrent immutable worker leases.
///
/// Pool mutation requires `&mut self`; admission/public release must be
/// serialized by its local controller. It never waits for readiness, GPU work,
/// or consumers. Released/draining cells count against both slot and byte limits.
pub struct FrameResources {
    identity: Arc<()>,
    config: ResourceConfig,
    producers: Vec<Id>,
    inputs: Pool<FrameMetadata>,
    assets: Pool<ImageDescription>,
    namespace: u64,
    next_id: u64,
}

impl FrameResources {
    pub fn new(config: ResourceConfig, producers: Vec<Id>) -> Result<Self, ResourceError> {
        if config.native_extent.validate().is_err()
            || config.max_producers == 0
            || producers.is_empty()
            || producers.len() > config.max_producers
            || producers
                .iter()
                .enumerate()
                .any(|(i, stream)| producers[..i].contains(stream))
        {
            return Err(ResourceError::InvalidConfiguration);
        }
        Ok(Self {
            identity: Arc::new(()),
            inputs: Pool::new(config.max_frames, config.max_frame_bytes)?,
            assets: Pool::new(config.max_assets, config.max_asset_bytes)?,
            config,
            producers,
            namespace: NEXT_OWNER
                .try_update(Ordering::AcqRel, Ordering::Acquire, |next| {
                    next.checked_add(1)
                })
                .map_err(|_| ResourceError::Capacity)?,
            next_id: 0,
        })
    }

    pub fn runtime_id(&self) -> &Id {
        &self.config.runtime_id
    }

    pub fn producer(&self, stream: &Id) -> Result<Producer, ResourceError> {
        if !self.producers.contains(stream) {
            return Err(ResourceError::NotFound);
        }
        Ok(Producer {
            owner: Arc::clone(&self.identity),
            stream: stream.clone(),
        })
    }

    /// Copy tightly packed synthetic pixels. The guard models the registered
    /// producer's readiness and independent physical lifetime.
    pub fn register(
        &mut self,
        producer: &Producer,
        metadata: FrameMetadata,
        pixels: &[u8],
    ) -> Result<(WriteTicket, ProducerGuard<FrameMetadata>), ResourceError> {
        self.check_owner(&producer.owner)?;
        if metadata.frame.stream_id != producer.stream
            || metadata.image.extent != self.config.native_extent
            || metadata.timeline.timeline_id != metadata.position.timeline_id
            || metadata.timeline.validate().is_err()
            || metadata.position.validate().is_err()
        {
            return Err(ResourceError::IncompatibleInput);
        }
        validate_pixels(&metadata.image, pixels)?;
        let (ticket_id, image_id) = self.allocate_ids()?;
        let (key, guard) = self
            .inputs
            .reserve(metadata, pixels, ticket_id.clone(), image_id)?;
        Ok((
            WriteTicket {
                owner: Arc::clone(&self.identity),
                key,
                id: ticket_id,
            },
            guard,
        ))
    }

    /// Validate all supplied shared metadata before consuming the write ticket.
    /// Publication is allowed while the native readiness dependency is pending.
    pub fn publish(
        &mut self,
        ticket: &WriteTicket,
        metadata: &FrameMetadata,
    ) -> Result<ImageRef, ResourceError> {
        self.check_owner(&ticket.owner)?;
        self.inputs.publish(ticket.key, metadata)?;
        Ok(ImageRef {
            owner: Arc::clone(&self.identity),
            key: ticket.key,
            id: self.inputs.image_id(ticket.key)?.clone(),
        })
    }

    /// Resolve a registered common ticket ID within this owner's bounded cells.
    /// The controller supplies the authorized producer capability, not a client
    /// boolean or a producer identity inferred from possession of the string.
    pub fn publish_id(
        &mut self,
        producer: &Producer,
        ticket_id: &Id,
        metadata: &FrameMetadata,
    ) -> Result<ImageRef, ResourceError> {
        self.check_owner(&producer.owner)?;
        if metadata.frame.stream_id != producer.stream {
            return Err(ResourceError::IncompatibleInput);
        }
        let key = self
            .inputs
            .key_for_id(ticket_id, false)
            .ok_or(ResourceError::NotFound)?;
        self.inputs.publish(key, metadata)?;
        Ok(ImageRef {
            owner: Arc::clone(&self.identity),
            key,
            id: self.inputs.image_id(key)?.clone(),
        })
    }

    /// Cancel an unpublished registration; this is NOT producer completion.
    pub fn cancel_write(&mut self, ticket: &WriteTicket) -> Result<bool, ResourceError> {
        self.check_owner(&ticket.owner)?;
        Ok(self.inputs.revoke(ticket.key, false))
    }

    /// Retain input independently of its publisher, even before write readiness.
    pub fn acquire(&self, image: &ImageRef) -> Result<Lease<FrameMetadata>, ResourceError> {
        self.check_owner(&image.owner)?;
        self.inputs.acquire(image.key)
    }

    /// Same retention/readiness semantics as `acquire`, without an adapter map.
    pub fn acquire_id(&self, image_id: &Id) -> Result<Lease<FrameMetadata>, ResourceError> {
        let key = self
            .inputs
            .key_for_id(image_id, true)
            .ok_or(ResourceError::NotFound)?;
        self.inputs.acquire(key)
    }

    /// Revoke future acquisition. Existing leases and work guards remain valid.
    /// Repeated release is false; no tombstones or lifetime retry history is kept.
    pub fn release(&mut self, image: &ImageRef) -> Result<bool, ResourceError> {
        self.check_owner(&image.owner)?;
        Ok(self.inputs.revoke(image.key, true))
    }

    /// Revoke ID lookup and publisher ownership, never physical completion.
    pub fn release_id(&mut self, image_id: &Id) -> bool {
        self.inputs
            .key_for_id(image_id, true)
            .is_some_and(|key| self.inputs.revoke(key, true))
    }

    /// Register immutable, ready synthetic image content. No file/path adapter.
    pub fn register_asset(
        &mut self,
        image: ImageDescription,
        pixels: &[u8],
    ) -> Result<AssetRef, ResourceError> {
        validate_pixels(&image, pixels)?;
        let (ticket_id, image_id) = self.allocate_ids()?;
        let (key, guard) =
            self.assets
                .reserve(image.clone(), pixels, ticket_id, image_id.clone())?;
        guard.ready()?;
        guard.complete();
        self.assets.publish(key, &image)?;
        Ok(AssetRef {
            owner: Arc::clone(&self.identity),
            key,
            id: image_id,
        })
    }

    pub fn acquire_asset(
        &self,
        asset: &AssetRef,
    ) -> Result<Lease<ImageDescription>, ResourceError> {
        self.check_owner(&asset.owner)?;
        self.assets.acquire(asset.key)
    }

    pub fn release_asset(&mut self, asset: &AssetRef) -> Result<bool, ResourceError> {
        self.check_owner(&asset.owner)?;
        Ok(self.assets.revoke(asset.key, true))
    }

    pub fn acquire_asset_id(
        &self,
        asset_id: &Id,
    ) -> Result<Lease<ImageDescription>, ResourceError> {
        let key = self
            .assets
            .key_for_id(asset_id, true)
            .ok_or(ResourceError::NotFound)?;
        self.assets.acquire(key)
    }

    pub fn release_asset_id(&mut self, asset_id: &Id) -> bool {
        self.assets
            .key_for_id(asset_id, true)
            .is_some_and(|key| self.assets.revoke(key, true))
    }

    /// Reclaim physically finished, unreferenced cells. Registration also sweeps.
    pub fn reclaim(&mut self) {
        self.inputs.reclaim();
        self.assets.reclaim();
    }

    /// Physical occupancy, including revoked/cancelled draining work.
    pub fn occupied_frames(&self) -> usize {
        self.inputs.occupied()
    }

    pub fn occupied_assets(&self) -> usize {
        self.assets.occupied()
    }

    fn check_owner(&self, owner: &Arc<()>) -> Result<(), ResourceError> {
        if Arc::ptr_eq(owner, &self.identity) {
            Ok(())
        } else {
            Err(ResourceError::NotFound)
        }
    }

    fn allocate_ids(&mut self) -> Result<(Id, Id), ResourceError> {
        let serial = self.next_id;
        self.next_id = serial.checked_add(1).ok_or(ResourceError::Capacity)?;
        let prefix = format!(
            "native:{}:{}:{serial}",
            self.config.runtime_id.as_str(),
            self.namespace
        );
        let ticket = Id::try_from(format!("{prefix}:write"))
            .map_err(|_| ResourceError::InvalidConfiguration)?;
        let image = Id::try_from(format!("{prefix}:image"))
            .map_err(|_| ResourceError::InvalidConfiguration)?;
        Ok((ticket, image))
    }
}

fn validate_pixels(image: &ImageDescription, pixels: &[u8]) -> Result<(), ResourceError> {
    image.validate().map_err(|_| ResourceError::InvalidPixels)?;
    let stride = match image.format {
        ImageFormat::Rgba8Unorm | ImageFormat::Bgra8Unorm => 4,
        ImageFormat::Rgba16Float => 8,
    };
    let expected = (image.extent.width as usize)
        .checked_mul(image.extent.height as usize)
        .and_then(|count| count.checked_mul(stride))
        .ok_or(ResourceError::InvalidPixels)?;
    if pixels.len() != expected {
        return Err(ResourceError::InvalidPixels);
    }
    // IEEE binary16, little endian: all-ones exponent means infinity or NaN.
    if image.format == ImageFormat::Rgba16Float
        && pixels
            .as_chunks::<2>()
            .0
            .iter()
            .any(|value| u16::from_le_bytes(*value) & 0x7c00 == 0x7c00)
    {
        return Err(ResourceError::InvalidPixels);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
