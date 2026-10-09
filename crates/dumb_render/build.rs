//! Compile WGSL shaders to SPIR-V at build time with naga (no Vulkan SDK required).

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    for name in ["mesh", "line", "egui", "sky"] {
        let path = format!("shaders/{name}.wgsl");
        println!("cargo:rerun-if-changed={path}");
        let src = std::fs::read_to_string(&path).unwrap();
        let module = match naga::front::wgsl::parse_str(&src) {
            Ok(m) => m,
            Err(e) => panic!("{}", e.emit_to_string_with_path(&src, &path)),
        };
        let info = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::PUSH_CONSTANT,
        )
        .validate(&module)
        .unwrap_or_else(|e| panic!("{}", e.emit_to_string_with_path(&src, &path)));
        let options = naga::back::spv::Options {
            // We flip Y in the projection matrix ourselves.
            flags: naga::back::spv::WriterFlags::empty(),
            ..Default::default()
        };
        let words = naga::back::spv::write_vec(&module, &info, &options, None).expect("SPIR-V generation failed");
        let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        std::fs::write(out.join(format!("{name}.spv")), bytes).unwrap();
    }
}
