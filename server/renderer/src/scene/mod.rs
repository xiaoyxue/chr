//! GPU-independent retained scene state. Runtime owns when mutations take effect;
//! a [`SceneView`] owns an immutable version and keeps its assets alive.
//! Handles are process-local and scene-scoped, never protocol/resource IDs.

mod appearance;
mod geometry;
mod storage;
mod texture;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

pub use appearance::{EnvironmentMap, Light, Lighting, PbrMaterial, SkyLight};
pub use geometry::{Aabb, MeshAttributes, MeshData, ModelMatrix};
use storage::{Key, Store};
pub use texture::{Filter, MaterialMaps, TextureData, TextureSampler, Wrap};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SceneError {
    #[error("handle is stale or belongs to another scene")]
    InvalidHandle,
    #[error("resource is still referenced by an instance")]
    ResourceInUse,
    #[error("instance requires one valid material per mesh part")]
    MaterialBindings,
    #[error("geometry and transformed results must be finite")]
    NonFinite,
    #[error("bounds must be finite and ordered")]
    InvalidBounds,
    #[error("model matrix must be affine with last row [0, 0, 0, 1]")]
    NonAffineModel,
    #[error("model matrix must have an invertible linear part")]
    SingularModel,
    #[error("normal or direction must be nonzero")]
    ZeroDirection,
    #[error("mesh must contain finite vertices and valid indexed triangles")]
    InvalidMesh,
    #[error("mesh parts must partition the complete index array into nonempty triangle ranges")]
    InvalidMeshParts,
    #[error("mesh attributes must be finite and match the position count")]
    InvalidMeshAttributes,
    #[error("texture dimensions must match RGBA8 data")]
    InvalidTexture,
    #[error("material factors are outside their finite physical ranges")]
    InvalidMaterial,
    #[error("lighting has invalid emission, position, direction, or cone parameters")]
    InvalidLighting,
    #[error("environment dimensions and finite nonnegative RGB radiance must agree")]
    InvalidEnvironment,
    #[error("scene identity or revision exhausted")]
    IdentityExhausted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MeshId(Key);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MaterialId(Key);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InstanceId(Key);

/// Material bindings are per instance and correspond to the mesh's part order.
#[derive(Clone, Debug)]
pub struct Instance {
    mesh: MeshId,
    materials: Arc<[MaterialId]>,
    model: ModelMatrix,
    bounds: Aabb,
    visible: bool,
}

impl Instance {
    pub fn mesh(&self) -> MeshId {
        self.mesh
    }

    pub fn materials(&self) -> &[MaterialId] {
        &self.materials
    }

    pub fn model(&self) -> ModelMatrix {
        self.model
    }

    pub fn bounds(&self) -> Aabb {
        self.bounds
    }

    pub fn visible(&self) -> bool {
        self.visible
    }
}

#[derive(Clone, Debug)]
struct State {
    revision: u64,
    meshes: Store<MeshData>,
    materials: Store<MaterialEntry>,
    instances: Store<Instance>,
    lighting: Lighting,
}

#[derive(Clone, Debug)]
struct MaterialEntry {
    factors: PbrMaterial,
    maps: MaterialMaps,
}

/// Not Clone: independently mutable scenes must never share a handle namespace.
#[derive(Debug)]
pub struct Scene {
    state: Arc<State>,
}

/// Owned, immutable view. Later updates/removals cannot change this version.
#[derive(Clone, Debug)]
pub struct SceneView {
    state: Arc<State>,
}

impl SceneView {
    /// Monotonic within this scene only; not a source frame identity or timestamp.
    pub fn revision(&self) -> u64 {
        self.state.revision
    }

    pub fn mesh(&self, id: MeshId) -> Result<&MeshData, SceneError> {
        self.state.meshes.get(id.0).map(Arc::as_ref)
    }

    pub fn material(&self, id: MaterialId) -> Result<&PbrMaterial, SceneError> {
        Ok(&self.state.materials.get(id.0)?.factors)
    }

    pub fn material_maps(&self, id: MaterialId) -> Result<&MaterialMaps, SceneError> {
        Ok(&self.state.materials.get(id.0)?.maps)
    }

    pub fn instance(&self, id: InstanceId) -> Result<&Instance, SceneError> {
        self.state.instances.get(id.0).map(Arc::as_ref)
    }

    pub fn instances(&self) -> impl Iterator<Item = (InstanceId, &Instance)> {
        self.state
            .instances
            .iter()
            .map(|(id, value)| (InstanceId(id), value.as_ref()))
    }

    pub fn lighting(&self) -> &Lighting {
        &self.state.lighting
    }
}

impl Scene {
    pub fn new() -> Result<Self, SceneError> {
        static NEXT_SCENE: AtomicU64 = AtomicU64::new(0);
        let owner = NEXT_SCENE
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |v| v.checked_add(1))
            .map_err(|_| SceneError::IdentityExhausted)?;
        Ok(Self {
            state: Arc::new(State {
                revision: 0,
                meshes: Store::new(owner),
                materials: Store::new(owner),
                instances: Store::new(owner),
                lighting: Lighting::default(),
            }),
        })
    }

    pub fn snapshot(&self) -> SceneView {
        SceneView {
            state: Arc::clone(&self.state),
        }
    }

    pub fn add_mesh(&mut self, mesh: MeshData) -> Result<MeshId, SceneError> {
        self.update(|state| Ok(MeshId(state.meshes.insert(mesh))))
    }

    /// Active instances, even hidden ones, prevent deletion. Old views retain their assets.
    pub fn remove_mesh(&mut self, id: MeshId) -> Result<(), SceneError> {
        if self.state.instances.iter().any(|(_, i)| i.mesh == id) {
            return Err(SceneError::ResourceInUse);
        }
        self.update(|state| state.meshes.remove(id.0))
    }

    pub fn add_material(&mut self, material: PbrMaterial) -> Result<MaterialId, SceneError> {
        self.add_material_with_maps(material, MaterialMaps::default())
    }

    pub fn add_material_with_maps(
        &mut self,
        material: PbrMaterial,
        maps: MaterialMaps,
    ) -> Result<MaterialId, SceneError> {
        material.validate()?;
        self.update(|state| {
            Ok(MaterialId(state.materials.insert(MaterialEntry {
                factors: material,
                maps,
            })))
        })
    }

    /// Keeps identity while preserving the material value in older views.
    pub fn set_material(
        &mut self,
        id: MaterialId,
        material: PbrMaterial,
    ) -> Result<(), SceneError> {
        material.validate()?;
        let mut entry = self.state.materials.get(id.0)?.as_ref().clone();
        entry.factors = material;
        self.update(|state| state.materials.replace(id.0, entry))
    }

    pub fn set_material_maps(
        &mut self,
        id: MaterialId,
        maps: MaterialMaps,
    ) -> Result<(), SceneError> {
        let mut entry = self.state.materials.get(id.0)?.as_ref().clone();
        entry.maps = maps;
        self.update(|state| state.materials.replace(id.0, entry))
    }

    pub fn remove_material(&mut self, id: MaterialId) -> Result<(), SceneError> {
        if self
            .state
            .instances
            .iter()
            .any(|(_, i)| i.materials.contains(&id))
        {
            return Err(SceneError::ResourceInUse);
        }
        self.update(|state| state.materials.remove(id.0))
    }

    pub fn add_instance(
        &mut self,
        mesh: MeshId,
        materials: Vec<MaterialId>,
        model: ModelMatrix,
    ) -> Result<InstanceId, SceneError> {
        let geometry = self.state.meshes.get(mesh.0)?;
        self.validate_bindings(geometry, &materials)?;
        let instance = Instance {
            mesh,
            materials: materials.into(),
            model,
            bounds: geometry.bounds().transformed(model)?,
            visible: true,
        };
        self.update(|state| Ok(InstanceId(state.instances.insert(instance))))
    }

    pub fn remove_instance(&mut self, id: InstanceId) -> Result<(), SceneError> {
        self.update(|state| state.instances.remove(id.0))
    }

    pub fn set_model(&mut self, id: InstanceId, model: ModelMatrix) -> Result<(), SceneError> {
        let mut instance = self.state.instances.get(id.0)?.as_ref().clone();
        instance.bounds = self
            .state
            .meshes
            .get(instance.mesh.0)?
            .bounds()
            .transformed(model)?;
        instance.model = model;
        self.update(|state| state.instances.replace(id.0, instance))
    }

    pub fn set_visible(&mut self, id: InstanceId, visible: bool) -> Result<(), SceneError> {
        let mut instance = self.state.instances.get(id.0)?.as_ref().clone();
        instance.visible = visible;
        self.update(|state| state.instances.replace(id.0, instance))
    }

    pub fn set_material_bindings(
        &mut self,
        id: InstanceId,
        materials: Vec<MaterialId>,
    ) -> Result<(), SceneError> {
        let mut instance = self.state.instances.get(id.0)?.as_ref().clone();
        self.validate_bindings(self.state.meshes.get(instance.mesh.0)?, &materials)?;
        instance.materials = materials.into();
        self.update(|state| state.instances.replace(id.0, instance))
    }

    pub fn set_lighting(&mut self, lighting: Lighting) -> Result<(), SceneError> {
        self.update(|state| {
            state.lighting = lighting;
            Ok(())
        })
    }

    fn validate_bindings(
        &self,
        mesh: &MeshData,
        materials: &[MaterialId],
    ) -> Result<(), SceneError> {
        if materials.len() != mesh.parts().len() {
            return Err(SceneError::MaterialBindings);
        }
        for material in materials {
            self.state.materials.get(material.0)?;
        }
        Ok(())
    }

    fn update<T>(
        &mut self,
        apply: impl FnOnce(&mut State) -> Result<T, SceneError>,
    ) -> Result<T, SceneError> {
        let next = self
            .state
            .revision
            .checked_add(1)
            .ok_or(SceneError::IdentityExhausted)?;
        let state = Arc::make_mut(&mut self.state);
        let result = apply(state)?;
        state.revision = next;
        Ok(result)
    }
}
