//! Writes a demo scene + materials into a project.
//! `cargo run -p dumb_runtime --example make_demo -- project`

use dumb_asset::{AssetDatabase, MaterialData};
use dumb_core::{builtin, Color, Vec3};
use dumb_ecs::{Animator, Camera, CharacterController, Collider, ColliderShape, Light, LightKind, MeshRenderer, RigidBody, SceneData, Transform, World};
use dumb_reflect::Value;

fn s(fields: &[(&str, Value)]) -> Value {
    Value::Struct(fields.iter().map(|(k, v)| (k.to_string(), v.clone())).collect())
}

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "project".into());
    let mut db = AssetDatabase::open(&dir).expect("open project");

    let mat = |db: &mut AssetDatabase, name: &str, m: MaterialData| {
        let rel = format!("Materials/{name}.mat");
        db.write_asset(&rel, &m.to_ron()).expect("write material")
    };
    let gold = mat(&mut db, "Gold", MaterialData { albedo: Color::rgb(1.0, 0.76, 0.33), metallic: 1.0, roughness: 0.25, ..Default::default() });
    let glow = mat(&mut db, "Glow", MaterialData { albedo: Color::rgb(0.05, 0.05, 0.05), emission: Color::rgb(0.2, 0.8, 1.0), emission_strength: 6.0, ..Default::default() });
    let red = mat(&mut db, "RedPlastic", MaterialData { albedo: Color::rgb(0.8, 0.08, 0.06), roughness: 0.35, ..Default::default() });
    let robot = db.id_for_path("Characters/Player/Robot.blend").expect("run tools/make_robot.py first");

    let mut w = World::new();
    let sun = w.spawn_named("Sun");
    w.get_mut::<Transform>(sun).unwrap().translation = Vec3::new(0.0, 12.0, 0.0);
    w.get_mut::<Transform>(sun).unwrap().set_euler_degrees(Vec3::new(-48.0, -40.0, 0.0));
    w.insert(sun, Light { kind: LightKind::Directional, intensity: 3.2, ..Default::default() });

    let cam = w.spawn_named("Main Camera");
    let t = w.get_mut::<Transform>(cam).unwrap();
    t.translation = Vec3::new(0.0, 5.5, 11.0);
    t.look_at(Vec3::new(0.0, 1.0, 0.0), Vec3::Y);
    w.insert(cam, Camera::default());

    let ground = w.spawn_named("Ground");
    w.get_mut::<Transform>(ground).unwrap().scale = Vec3::splat(8.0);
    w.insert(ground, MeshRenderer { model: builtin::PLANE, tint: Color::rgb(0.32, 0.34, 0.36), ..Default::default() });
    w.insert(ground, Collider::fitted(ColliderShape::Box));

    let player = w.spawn_named("Player");
    w.insert(player, MeshRenderer { model: robot, ..Default::default() });
    w.insert(player, Animator { clip: "Walk".into(), ..Default::default() });
    w.insert(player, CharacterController { height: 2.0, radius: 0.4, ..Default::default() });
    w.insert_by_name(player, "game_scripts::Thrower", &s(&[]));
    w.insert_by_name(player, "game_scripts::Player", &s(&[("speed", Value::Float(4.0)), ("health", Value::Float(100.0))]));

    let waver = w.spawn_named("Robot (waving)");
    w.get_mut::<Transform>(waver).unwrap().translation = Vec3::new(-3.0, 0.0, -1.0);
    w.insert(waver, MeshRenderer { model: robot, ..Default::default() });
    w.insert(waver, Animator { clip: "Wave".into(), ..Default::default() });
    w.insert(waver, Collider::capsule(0.8, 2.0));
    w.get_mut::<Collider>(waver).unwrap().offset = Vec3::new(0.0, 1.0, 0.0);

    let orb = w.spawn_named("Gold Orb");
    let t = w.get_mut::<Transform>(orb).unwrap();
    t.translation = Vec3::new(3.0, 1.2, 0.0);
    t.scale = Vec3::splat(1.4);
    w.insert(orb, MeshRenderer { model: builtin::SPHERE, material: gold, ..Default::default() });
    // Kinematic: moved by its Bobber script, pushes dynamic bodies around.
    w.insert(orb, RigidBody::kinematic());
    w.insert(orb, Collider::fitted(ColliderShape::Sphere));
    w.insert_by_name(orb, "game_scripts::Bobber", &s(&[("amplitude", Value::Float(0.3)), ("frequency", Value::Float(0.6))]));

    let cube = w.spawn_named("Spinning Cube");
    let t = w.get_mut::<Transform>(cube).unwrap();
    t.translation = Vec3::new(3.0, 3.0, 0.0);
    t.scale = Vec3::splat(0.6);
    w.insert(cube, MeshRenderer { model: builtin::CUBE, material: glow, ..Default::default() });
    w.insert_by_name(cube, "game_scripts::Spinner", &s(&[("degrees_per_second", Value::Float(120.0))]));
    w.set_parent(cube, Some(orb));

    let pillar = w.spawn_named("Pillar");
    let t = w.get_mut::<Transform>(pillar).unwrap();
    t.translation = Vec3::new(-3.5, 1.5, -4.0);
    t.scale = Vec3::new(0.8, 3.0, 0.8);
    w.insert(pillar, MeshRenderer { model: builtin::CYLINDER, material: red, ..Default::default() });
    w.insert(pillar, Collider::fitted(ColliderShape::Cylinder));

    // Physics playground: a stack of crates and some balls that tumble when play starts.
    let playground = w.spawn_named("Physics Playground");
    w.get_mut::<Transform>(playground).unwrap().translation = Vec3::new(6.0, 0.0, -4.0);
    for layer in 0..5 {
        for i in 0..(5 - layer) {
            let c = w.spawn_named(&format!("Crate {layer}-{i}"));
            let x = i as f32 * 1.02 + layer as f32 * 0.51 - 2.0;
            w.get_mut::<Transform>(c).unwrap().translation = Vec3::new(x, 0.5 + layer as f32 * 1.0, 0.0);
            let tint = if (layer + i) % 2 == 0 { Color::rgb(0.75, 0.5, 0.25) } else { Color::rgb(0.6, 0.38, 0.2) };
            w.insert(c, MeshRenderer { model: builtin::CUBE, tint, ..Default::default() });
            w.insert(c, RigidBody::dynamic());
            w.insert(c, Collider::fitted(ColliderShape::Box));
            w.set_parent(c, Some(playground));
        }
    }
    for i in 0..6 {
        let b = w.spawn_named(&format!("Ball {i}"));
        let t = w.get_mut::<Transform>(b).unwrap();
        t.translation = Vec3::new(-1.5 + i as f32 * 0.6, 7.0 + i as f32 * 1.5, 0.3 * (i % 2) as f32);
        t.scale = Vec3::splat(0.6);
        w.insert(b, MeshRenderer { model: builtin::SPHERE, tint: Color::rgb(0.2, 0.6, 1.0), ..Default::default() });
        w.insert(b, RigidBody::dynamic());
        w.insert(b, Collider { restitution: 0.7, ..Collider::fitted(ColliderShape::Sphere) });
        w.set_parent(b, Some(playground));
    }

    for (i, (p, c)) in [(Vec3::new(2.0, 1.5, 2.5), Color::rgb(1.0, 0.4, 0.1)), (Vec3::new(-2.5, 1.5, 1.5), Color::rgb(0.2, 0.5, 1.0))].into_iter().enumerate() {
        let l = w.spawn_named(&format!("Point Light {}", i + 1));
        w.get_mut::<Transform>(l).unwrap().translation = p;
        w.insert(l, Light { kind: LightKind::Point, color: c, intensity: 8.0, range: 8.0 });
    }

    let field = w.spawn_named("Procedural Field");
    w.get_mut::<Transform>(field).unwrap().translation = Vec3::new(0.0, 0.0, -75.0);
    w.insert_by_name(field, "game_scripts::ProceduralField", &s(&[("size", Value::Int(100)), ("shape", Value::Enum("Waves".into()))]));

    w.update_transforms();
    let data = SceneData::capture(&w);
    db.write_asset("Scenes/Main.scene", &data.to_ron().unwrap()).expect("write scene");

    let mut project = dumb_runtime::Project::open(&dir);
    project.settings.name = "Dumb Demo".into();
    project.settings.startup_scene = "Scenes/Main.scene".into();
    project.save().unwrap();
    println!("demo scene written ({} entities)", w.entity_count());
}
