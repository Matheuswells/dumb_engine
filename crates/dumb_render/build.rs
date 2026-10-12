//! Compile WGSL shaders to SPIR-V at build time with naga (no Vulkan SDK required).

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // (output name, source files concatenated in order)
    let shaders: [(&str, &[&str]); 5] = [
        ("mesh", &["common", "mesh"]),
        ("ink", &["common", "ink"]),
        ("line", &["line"]),
        ("egui", &["egui"]),
        ("sky", &["sky"]),
    ];
    for name in ["common", "mesh", "ink", "line", "egui", "sky", "surface"] {
        println!("cargo:rerun-if-changed=shaders/{name}.wgsl");
    }
    for (name, parts) in shaders {
        let path = format!("shaders/{name}.wgsl");
        let src: String = parts.iter().map(|p| std::fs::read_to_string(format!("shaders/{p}.wgsl")).unwrap() + "\n").collect();
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
