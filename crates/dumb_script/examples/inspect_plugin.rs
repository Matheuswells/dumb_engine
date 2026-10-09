//! Build-and-inspect a scripts crate: `cargo run -p dumb_script --example inspect_plugin -- <Scripts dir> <package> [NewScriptName]`
//! Optionally creates a script first, then loads the DLL and lists what it registers.

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("scripts dir"));
    let package = args.next().expect("package");
    if let Some(name) = args.next() {
        let p = dumb_script::project::create_script(&dir, &name, dumb_script::project::ScriptTemplate::Behaviour).unwrap();
        println!("created {}", p.display());
    }
    let mut host = dumb_script::ScriptHost::new(&dir, &package, std::env::temp_dir().join("dumb_inspect_cache"));
    host.start_build();
    let mut w = dumb_ecs::World::new();
    while host.is_building() || host.status == dumb_script::ScriptStatus::Building {
        host.update(&mut [&mut w]);
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    for m in &host.messages {
        println!("{:?} {}:{} {}", m.level, m.file.display(), m.line, m.text);
    }
    println!("status: {:?}", host.status);
    println!("components: {:?}", host.component_names());
    println!("systems: {:?}", host.systems().iter().map(|s| &s.name).collect::<Vec<_>>());
    host.unload(&mut [&mut w]);
}
