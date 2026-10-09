//! Asset pipeline: database, importers (glTF, Blender, images), materials, models.

pub mod autotex;
pub mod database;
pub mod import_blender;
pub mod import_gltf;
pub mod material;
pub mod model;
pub mod primitives;
pub mod processing;
pub mod texture;

pub use database::{AssetDatabase, AssetEntry, AssetEvent, AssetKind, AssetMeta, ImportSettings, LoadState, LodSettings, NormalsMode, Pivot};
pub use material::{AlphaMode, MaterialData, NormalFormat, RoughnessSource};
pub use model::{AnimationClip, AnimationEvent, ModelData, Trs, Vertex};
pub use texture::TextureData;
