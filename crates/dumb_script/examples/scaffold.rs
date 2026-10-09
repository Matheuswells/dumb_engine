//! Create a scripts crate from the command line:
//! `cargo run -p dumb_script --example scaffold -- <project>/Scripts <package_name>`

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("scripts dir"));
    let package = args.next().unwrap_or_else(|| "game_scripts".into());
    let engine_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    dumb_script::project::create_scripts_crate(&dir, &package, &engine_root, Some(&engine_root.join("Cargo.lock"))).unwrap();
    println!("scripts crate `{package}` written to {}", dir.display());
}
