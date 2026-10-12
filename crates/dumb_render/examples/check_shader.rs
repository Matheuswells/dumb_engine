//! Compile surface shaders the way the renderer does and print their errors.
//!
//!     cargo run -p dumb_render --example check_shader -- Assets/Shaders/Toon.wgsl ...

fn main() {
    let mut failed = false;
    for path in std::env::args().skip(1) {
        match std::fs::read_to_string(&path) {
            Ok(src) => {
                let errors = dumb_render::custom_shader::check(&src);
                if errors.is_empty() {
                    println!("{path}: ok");
                }
                for e in errors {
                    failed = true;
                    println!("{path}: {e}");
                }
            }
            Err(e) => {
                failed = true;
                println!("{path}: {e}");
            }
        }
    }
    std::process::exit(failed as i32);
}
