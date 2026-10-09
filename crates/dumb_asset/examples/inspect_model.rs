//! Import a model through the asset pipeline and print what was found.
//! `cargo run -p dumb_asset --example inspect_model -- <project_dir> <Assets-relative path>`

fn main() {
    let mut args = std::env::args().skip(1);
    let project = args.next().expect("project dir");
    let rel = args.next().expect("asset path");
    let mut db = dumb_asset::AssetDatabase::open(&project).expect("open project");
    let id = db.id_for_path(&rel).expect("asset not found");
    let t0 = std::time::Instant::now();
    let model = loop {
        db.update();
        if let Some(m) = db.model(id) {
            break m;
        }
        if let Some(dumb_asset::LoadState::Failed(e)) = db.entry(id).map(|e| e.state.clone()) {
            panic!("import failed: {e}");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    println!("imported in {:.2}s ({})", t0.elapsed().as_secs_f32(), model.source_format);
    println!("nodes={} meshes={} materials={:?} textures={}", model.nodes.len(), model.meshes.len(), model.material_names, model.textures.len());
    println!("verts={} tris={} aabb={:?}", model.vertex_count(), model.triangle_count(), model.aabb());
    for s in &model.skins {
        let names: Vec<&str> = s.joints.iter().map(|j| model.nodes[*j].name.as_str()).collect();
        println!("skin {} joints={:?}", s.name, names);
    }
    for a in &model.animations {
        println!("clip {} duration={:.2}s channels={}", a.name, a.duration, a.channels.len());
    }
    println!("collision nodes={:?}", model.collision_nodes.iter().map(|n| &model.nodes[*n].name).collect::<Vec<_>>());
}
