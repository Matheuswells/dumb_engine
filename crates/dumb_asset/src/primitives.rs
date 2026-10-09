//! Built-in procedural primitive models with fixed asset ids.

use crate::import_gltf::compute_tangents;
use crate::model::*;
use dumb_core::{Aabb, AssetId, Vec3};
use std::f32::consts::{PI, TAU};

pub use dumb_core::builtin::{is_builtin, CUBE, CYLINDER, DEFAULT_MATERIAL, PLANE, SPHERE};

pub const BUILTIN_MODELS: &[(AssetId, &str)] =
    &[(CUBE, "Cube"), (SPHERE, "Sphere"), (PLANE, "Plane"), (CYLINDER, "Cylinder")];

pub fn builtin_model(id: AssetId) -> Option<ModelData> {
    let (name, prim) = match id {
        CUBE => ("Cube", cube()),
        SPHERE => ("Sphere", sphere(48, 24)),
        PLANE => ("Plane", plane(10.0)),
        CYLINDER => ("Cylinder", cylinder(32)),
        _ => return None,
    };
    Some(single_mesh_model(name, prim))
}

pub fn single_mesh_model(name: &str, mut prim: Primitive) -> ModelData {
    compute_tangents(&mut prim.vertices, &prim.indices);
    prim.aabb = Aabb::from_points(prim.vertices.iter().map(|v| Vec3::from(v.position)));
    ModelData {
        nodes: vec![Node { name: name.into(), mesh: Some(0), ..Default::default() }],
        roots: vec![0],
        meshes: vec![MeshData { name: name.into(), primitives: vec![prim] }],
        source_format: "builtin".into(),
        ..Default::default()
    }
}

fn v(p: Vec3, n: Vec3, uv: [f32; 2]) -> Vertex {
    Vertex { position: p.to_array(), normal: n.to_array(), uv, ..Default::default() }
}

pub fn cube() -> Primitive {
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    let faces = [
        (Vec3::X, Vec3::Y),
        (Vec3::NEG_X, Vec3::Y),
        (Vec3::Y, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::Z),
        (Vec3::Z, Vec3::Y),
        (Vec3::NEG_Z, Vec3::Y),
    ];
    for (n, up) in faces {
        let right = up.cross(n);
        let base = verts.len() as u32;
        for (i, (sx, sy)) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].iter().enumerate() {
            let p = (n + right * *sx + up * *sy) * 0.5;
            let uv = [[0.0, 1.0], [1.0, 1.0], [1.0, 0.0], [0.0, 0.0]][i];
            verts.push(v(p, n, uv));
        }
        indices.extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    Primitive { vertices: verts, indices, material: None, aabb: Aabb::EMPTY, lods: Vec::new() }
}

pub fn sphere(segments: u32, rings: u32) -> Primitive {
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    for r in 0..=rings {
        let phi = PI * r as f32 / rings as f32;
        for s in 0..=segments {
            let theta = TAU * s as f32 / segments as f32;
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            verts.push(v(n * 0.5, n, [s as f32 / segments as f32, r as f32 / rings as f32]));
        }
    }
    let stride = segments + 1;
    for r in 0..rings {
        for s in 0..segments {
            let a = r * stride + s;
            let b = a + stride;
            indices.extend_from_slice(&[a, a + 1, b, a + 1, b + 1, b]);
        }
    }
    Primitive { vertices: verts, indices, material: None, aabb: Aabb::EMPTY, lods: Vec::new() }
}

pub fn plane(size: f32) -> Primitive {
    let h = size * 0.5;
    let verts = vec![
        v(Vec3::new(-h, 0.0, -h), Vec3::Y, [0.0, 0.0]),
        v(Vec3::new(h, 0.0, -h), Vec3::Y, [size, 0.0]),
        v(Vec3::new(h, 0.0, h), Vec3::Y, [size, size]),
        v(Vec3::new(-h, 0.0, h), Vec3::Y, [0.0, size]),
    ];
    Primitive { vertices: verts, indices: vec![0, 2, 1, 0, 3, 2], material: None, aabb: Aabb::EMPTY, lods: Vec::new() }
}

pub fn cylinder(segments: u32) -> Primitive {
    let mut verts = Vec::new();
    let mut indices = Vec::new();
    for s in 0..=segments {
        let t = TAU * s as f32 / segments as f32;
        let n = Vec3::new(t.cos(), 0.0, t.sin());
        let u = s as f32 / segments as f32;
        verts.push(v(n * 0.5 + Vec3::Y * 0.5, n, [u, 0.0]));
        verts.push(v(n * 0.5 - Vec3::Y * 0.5, n, [u, 1.0]));
    }
    for s in 0..segments {
        let a = s * 2;
        indices.extend_from_slice(&[a, a + 2, a + 1, a + 1, a + 2, a + 3]);
    }
    for (y, n) in [(0.5f32, Vec3::Y), (-0.5, Vec3::NEG_Y)] {
        let center = verts.len() as u32;
        verts.push(v(Vec3::new(0.0, y, 0.0), n, [0.5, 0.5]));
        for s in 0..=segments {
            let t = TAU * s as f32 / segments as f32;
            verts.push(v(Vec3::new(t.cos() * 0.5, y, t.sin() * 0.5), n, [t.cos() * 0.5 + 0.5, t.sin() * 0.5 + 0.5]));
        }
        for s in 0..segments {
            let (a, b) = (center + 1 + s, center + 2 + s);
            if y > 0.0 {
                indices.extend_from_slice(&[center, b, a]);
            } else {
                indices.extend_from_slice(&[center, a, b]);
            }
        }
    }
    Primitive { vertices: verts, indices, material: None, aabb: Aabb::EMPTY, lods: Vec::new() }
}
