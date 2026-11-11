use chr_common::{jsonrpc::ErrorObject, scene_frame as w};

use crate::retained::reject;

/// Bounded native staging, not a waiting service. No timeout, fallback, or wire
/// schema is introduced. The caller explicitly finishes a complete batch.
pub struct FrameJoin {
    capacity: usize,
    images: Vec<w::PublishedFrame>,
    geometry: Vec<(w::FrameKey, w::FrameGeometry)>,
}

impl FrameJoin {
    pub fn new(capacity: usize) -> Result<Self, ErrorObject> {
        if capacity == 0 {
            return Err(reject(
                w::RejectionKind::Capacity,
                "join capacity must be positive",
            ));
        }
        Ok(Self {
            capacity,
            images: Vec::new(),
            geometry: Vec::new(),
        })
    }
    pub fn image(&mut self, image: w::PublishedFrame) -> Result<(), ErrorObject> {
        if self.images.iter().any(|v| v.frame == image.frame) {
            return Err(reject(w::RejectionKind::Conflict, "duplicate join image"));
        }
        if self.images.len() == self.capacity {
            return Err(reject(
                w::RejectionKind::Capacity,
                "join image capacity exhausted",
            ));
        }
        self.images.push(image);
        Ok(())
    }
    pub fn geometry(
        &mut self,
        key: w::FrameKey,
        geometry: w::FrameGeometry,
    ) -> Result<(), ErrorObject> {
        if self.geometry.iter().any(|v| v.0 == key) {
            return Err(reject(
                w::RejectionKind::Conflict,
                "duplicate join geometry",
            ));
        }
        if self.geometry.len() == self.capacity {
            return Err(reject(
                w::RejectionKind::Capacity,
                "join geometry capacity exhausted",
            ));
        }
        self.geometry.push((key, geometry));
        Ok(())
    }
    /// Produces the caller's explicit order, never arrival order. Consumes the
    /// staging batch on failure too; the caller chooses any subsequent retry.
    pub fn finish(
        self,
        runtime: &w::Id,
        order: &[w::FrameKey],
    ) -> Result<Vec<w::FrameInput>, ErrorObject> {
        if order.is_empty() || order.len() > self.capacity {
            return Err(reject(
                w::RejectionKind::Capacity,
                "invalid join batch size",
            ));
        }
        order
            .iter()
            .enumerate()
            .map(|(i, key)| {
                if order[..i].contains(key) {
                    return Err(reject(
                        w::RejectionKind::Conflict,
                        "duplicate requested frame",
                    ));
                }
                let image = self
                    .images
                    .iter()
                    .find(|v| &v.frame == key)
                    .ok_or_else(|| reject(w::RejectionKind::NotFound, "missing join image"))?;
                if &image.runtime_id != runtime {
                    return Err(reject(
                        w::RejectionKind::RuntimeChanged,
                        "join runtime changed",
                    ));
                }
                let geometry = self.geometry.iter().find(|v| &v.0 == key).ok_or_else(|| {
                    reject(w::RejectionKind::IncompatibleInput, "missing join geometry")
                })?;
                Ok(w::FrameInput {
                    frame: key.clone(),
                    image_ref: image.image_ref.clone(),
                    position: image.position.clone(),
                    geometry: geometry.1.clone(),
                })
            })
            .collect()
    }
}
