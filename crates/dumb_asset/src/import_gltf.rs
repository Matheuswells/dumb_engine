//! glTF 2.0 (.gltf / .glb) importer — the native interchange format for Blender content.

use crate::material::{AlphaMode, MaterialData};
use crate::model::*;
use crate::texture::TextureData;
use dumb_core::{Aabb, AssetId, Color, Mat4, Quat, Vec3, Vec4};
use std::path::Path;

/// Import a glTF file. `owner` is the model's asset id; textures referenced by materials
/// become sub-assets `owner.sub("texture", i)`.
pub fn import(path: &Path, owner: AssetId, scale: f32) -> Result<ModelData, String> {
    let (doc, buffers, images) = gltf::import(path).map_err(|e| format!("glTF import failed: {e}"))?;
    let mut model = ModelData { source_format: "glTF".into(), ..Default::default() };

    for (i, img) in images.iter().enumerate() {
        let name = doc
            .images()
            .nth(i)
            .and_then(|im| im.name().map(str::to_string))
            .unwrap_or_else(|| format!("Texture{i}"));
        model.textures.push(TextureData::from_gltf(name, img));
    }

    let tex_id = |info: Option<gltf::texture::Texture>| -> AssetId {
        info.map(|t| owner.sub("texture", t.source().index())).unwrap_or(AssetId::NONE)
    };

    for (i, m) in doc.materials().enumerate() {
        let pbr = m.pbr_metallic_roughness();
        let bc = pbr.base_color_factor();
        let em = m.emissive_factor();
        let mat = MaterialData {
            albedo: Color::rgba(bc[0], bc[1], bc[2], bc[3]),
            albedo_texture: tex_id(pbr.base_color_texture().map(|t| t.texture())),
            normal_texture: tex_id(m.normal_texture().map(|t| t.texture())),
            normal_scale: m.normal_texture().map_or(1.0, |t| t.scale()),
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            metallic_roughness_texture: tex_id(pbr.metallic_roughness_texture().map(|t| t.texture())),
            ao_texture: tex_id(m.occlusion_texture().map(|t| t.texture())),
            ao_strength: m.occlusion_texture().map_or(1.0, |t| t.strength()),
            emission: Color::rgb(em[0], em[1], em[2]),
            emission_strength: m.emissive_strength().unwrap_or(1.0),
            emission_texture: tex_id(m.emissive_texture().map(|t| t.texture())),
            alpha_mode: match m.alpha_mode() {
                gltf::material::AlphaMode::Opaque => AlphaMode::Opaque,
                gltf::material::AlphaMode::Mask => AlphaMode::Mask,
                gltf::material::AlphaMode::Blend => AlphaMode::Blend,
            },
            alpha_cutoff: m.alpha_cutoff().unwrap_or(0.5),
            double_sided: m.double_sided(),
            ..Default::default()
        };
        model.materials.push(mat);
        model.material_names.push(m.name().map(str::to_string).unwrap_or_else(|| format!("Material{i}")));
    }

    for (mi, mesh) in doc.meshes().enumerate() {
        let mut out = MeshData {
            name: mesh.name().map(str::to_string).unwrap_or_else(|| format!("Mesh{mi}")),
            primitives: Vec::new(),
        };
        for prim in mesh.primitives() {
            if prim.mode() != gltf::mesh::Mode::Triangles {
                log::warn!("{}: skipping non-triangle primitive in mesh {}", path.display(), out.name);
                continue;
            }
            let reader = prim.reader(|b| Some(&buffers[b.index()]));
            let Some(positions) = reader.read_positions() else { continue };
            let mut verts: Vec<Vertex> = positions
                .map(|p| Vertex { position: (Vec3::from(p) * scale).to_array(), weights: [0.0; 4], ..Default::default() })
                .collect();
            if let Some(n) = reader.read_normals() {
                for (v, n) in verts.iter_mut().zip(n) {
                    v.normal = n;
                }
            }
            if let Some(uv) = reader.read_tex_coords(0) {
                for (v, uv) in verts.iter_mut().zip(uv.into_f32()) {
                    v.uv = uv;
                }
            }
            let has_tangents = if let Some(t) = reader.read_tangents() {
                for (v, t) in verts.iter_mut().zip(t) {
                    v.tangent = t;
                }
                true
            } else {
                false
            };
            if let Some(j) = reader.read_joints(0) {
                for (v, j) in verts.iter_mut().zip(j.into_u16()) {
                    v.joints = j;
                }
            }
            if let Some(w) = reader.read_weights(0) {
                for (v, w) in verts.iter_mut().zip(w.into_f32()) {
                    v.weights = w;
                }
            }
            let indices: Vec<u32> = match reader.read_indices() {
                Some(i) => i.into_u32().collect(),
                None => (0..verts.len() as u32).collect(),
            };
            if reader.read_normals().is_none() {
                compute_normals(&mut verts, &indices);
            }
            if !has_tangents {
                compute_tangents(&mut verts, &indices);
            }
            let aabb = Aabb::from_points(verts.iter().map(|v| Vec3::from(v.position)));
            out.primitives.push(Primitive { vertices: verts, indices, material: prim.material().index(), aabb, lods: Vec::new() });
        }
        model.meshes.push(out);
    }

    for (i, n) in doc.nodes().enumerate() {
        let (t, r, s) = n.transform().decomposed();
        model.nodes.push(Node {
            name: n.name().map(str::to_string).unwrap_or_else(|| format!("Node{i}")),
            parent: None,
            children: n.children().map(|c| c.index()).collect(),
            local: Trs { translation: Vec3::from(t) * scale, rotation: Quat::from_array(r), scale: Vec3::from(s) },
            mesh: n.mesh().map(|m| m.index()),
            skin: n.skin().map(|s| s.index()),
        });
    }
    for i in 0..model.nodes.len() {
        for c in model.nodes[i].children.clone() {
            model.nodes[c].parent = Some(i);
        }
    }
    model.roots = match doc.default_scene().or_else(|| doc.scenes().next()) {
        Some(s) => s.nodes().map(|n| n.index()).collect(),
        None => (0..model.nodes.len()).filter(|i| model.nodes[*i].parent.is_none()).collect(),
    };

    for (si, skin) in doc.skins().enumerate() {
        let reader = skin.reader(|b| Some(&buffers[b.index()]));
        let joints: Vec<usize> = skin.joints().map(|j| j.index()).collect();
        let inverse_bind: Vec<Mat4> = match reader.read_inverse_bind_matrices() {
            Some(m) => m
                .map(|m| {
                    let mut m = Mat4::from_cols_array_2d(&m);
                    m.w_axis = (m.w_axis.truncate() * scale).extend(m.w_axis.w);
                    m
                })
                .collect(),
            None => vec![Mat4::IDENTITY; joints.len()],
        };
        model.skins.push(Skin {
            name: skin.name().map(str::to_string).unwrap_or_else(|| format!("Skin{si}")),
            joints,
            inverse_bind,
        });
    }

    for (ai, anim) in doc.animations().enumerate() {
        let mut clip = AnimationClip {
            name: anim.name().map(str::to_string).unwrap_or_else(|| format!("Animation{ai}")),
            ..Default::default()
        };
        for ch in anim.channels() {
            let reader = ch.reader(|b| Some(&buffers[b.index()]));
            let Some(inputs) = reader.read_inputs() else { continue };
            let times: Vec<f32> = inputs.collect();
            let cubic = ch.sampler().interpolation() == gltf::animation::Interpolation::CubicSpline;
            let interpolation = match ch.sampler().interpolation() {
                gltf::animation::Interpolation::Step => Interpolation::Step,
                _ => Interpolation::Linear,
            };
            use gltf::animation::util::ReadOutputs;
            let (target, mut values): (ChannelTarget, Vec<Vec4>) = match reader.read_outputs() {
                Some(ReadOutputs::Translations(t)) => {
                    (ChannelTarget::Translation, t.map(|v| (Vec3::from(v) * scale).extend(0.0)).collect())
                }
                Some(ReadOutputs::Rotations(r)) => (ChannelTarget::Rotation, r.into_f32().map(Vec4::from).collect()),
                Some(ReadOutputs::Scales(s)) => (ChannelTarget::Scale, s.map(|v| Vec3::from(v).extend(0.0)).collect()),
                _ => continue,
            };
            if cubic {
                // in-tangent, value, out-tangent triplets: keep the values.
                values = values.chunks(3).filter_map(|c| c.get(1).copied()).collect();
            }
            if let Some(last) = times.last() {
                clip.duration = clip.duration.max(*last);
            }
            clip.channels.push(Channel { node: ch.target().node().index(), target, interpolation, times, values });
        }
        model.animations.push(clip);
    }

    model.classify_nodes();
    Ok(model)
}

pub(crate) fn compute_normals(verts: &mut [Vertex], indices: &[u32]) {
    let mut acc = vec![Vec3::ZERO; verts.len()];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let (pa, pb, pc) = (Vec3::from(verts[a].position), Vec3::from(verts[b].position), Vec3::from(verts[c].position));
        let n = (pb - pa).cross(pc - pa);
        acc[a] += n;
        acc[b] += n;
        acc[c] += n;
    }
    for (v, n) in verts.iter_mut().zip(acc) {
        v.normal = n.normalize_or(Vec3::Y).to_array();
    }
}

pub(crate) fn compute_tangents(verts: &mut [Vertex], indices: &[u32]) {
    let mut tan = vec![Vec3::ZERO; verts.len()];
    let mut bit = vec![Vec3::ZERO; verts.len()];
    for t in indices.chunks_exact(3) {
        let (a, b, c) = (t[0] as usize, t[1] as usize, t[2] as usize);
        let (pa, pb, pc) = (Vec3::from(verts[a].position), Vec3::from(verts[b].position), Vec3::from(verts[c].position));
        let (ua, ub, uc) = (verts[a].uv, verts[b].uv, verts[c].uv);
        let (e1, e2) = (pb - pa, pc - pa);
        let (du1, dv1, du2, dv2) = (ub[0] - ua[0], ub[1] - ua[1], uc[0] - ua[0], uc[1] - ua[1]);
        let det = du1 * dv2 - du2 * dv1;
        if det.abs() < 1e-12 {
            continue;
        }
        let r = 1.0 / det;
        let t = (e1 * dv2 - e2 * dv1) * r;
        let bt = (e2 * du1 - e1 * du2) * r;
        for i in [a, b, c] {
            tan[i] += t;
            bit[i] += bt;
        }
    }
    for (i, v) in verts.iter_mut().enumerate() {
        let n = Vec3::from(v.normal);
        let t = (tan[i] - n * n.dot(tan[i])).normalize_or(n.any_orthonormal_vector());
        let w = if n.cross(t).dot(bit[i]) < 0.0 { -1.0 } else { 1.0 };
        v.tangent = t.extend(w).to_array();
    }
}
