//! User surface shaders. A shader asset (`.wgsl`) defines `fn surface(s: Surface) -> vec4<f32>`
//! and optionally `fn vertex(v: Vertex) -> Vertex`; the engine adds the bindings, lighting
//! helpers and entry points (shaders/surface.wgsl + common.wgsl) and compiles the result to
//! SPIR-V at runtime with naga. The user's code goes first, so error line numbers match the
//! user's file.

static COMMON: &str = include_str!("../shaders/common.wgsl");
static SURFACE: &str = include_str!("../shaders/surface.wgsl");

/// A compile error. `line`/`column` are 1-based positions in the user's file; `line` is 0 when
/// the error is in the engine's part (usually a name the user's code redefines).
#[derive(Clone, Debug, PartialEq)]
pub struct ShaderError {
    pub line: usize,
    pub column: usize,
    pub message: String,
}

impl std::fmt::Display for ShaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if self.line > 0 {
            write!(f, "line {}:{}: {}", self.line, self.column, self.message)
        } else {
            write!(f, "{}", self.message)
        }
    }
}

/// The starting point for new shaders: the engine's standard lighting.
pub const TEMPLATE: &str = r#"// Surface shader.
//
// `surface` returns the final color of each pixel. `s` holds world_pos, normal, view_dir, uv,
// albedo (material color x albedo texture x object tint), pixel, object_pos and time.
//
// Helpers: lit(s, albedo, metallic, roughness) (standard lighting, linear HDR), tonemap(c),
// shadow(s), sun_direction(), sun_color(), ambient(n), sky(dir, blur), albedo_texture(uv),
// params0() / params1() (the material's "Shader params"), time(), camera_pos().
//
// Add `fn vertex(v: Vertex) -> Vertex` to move vertices in world space (waves, wind...).

fn surface(s: Surface) -> vec4<f32> {
    let color = lit(s, s.albedo.rgb, material_metallic(), material_roughness());
    return vec4<f32>(tonemap(color), s.albedo.a);
}
"#;

/// Whether the user's code defines `fn vertex(`.
fn defines_vertex(src: &str) -> bool {
    src.lines().any(|l| {
        let l = l.trim_start();
        l.strip_prefix("fn").is_some_and(|r| r.starts_with(char::is_whitespace) && r.trim_start().strip_prefix("vertex").is_some_and(|r| r.trim_start().starts_with('(')))
    })
}

/// The full module: user code, then the engine's parts.
pub fn compose(user: &str) -> String {
    let mut s = String::with_capacity(user.len() + COMMON.len() + SURFACE.len() + 128);
    s.push_str(user);
    s.push('\n');
    if !defines_vertex(user) {
        s.push_str("fn vertex(v: Vertex) -> Vertex { return v; }\n");
    }
    s.push_str(COMMON);
    s.push('\n');
    s.push_str(SURFACE);
    s
}

fn error_at(user: &str, full: &str, loc: Option<naga::SourceLocation>, message: String) -> ShaderError {
    let user_lines = user.lines().count();
    match loc {
        Some(l) if (l.line_number as usize) <= user_lines => ShaderError { line: l.line_number as usize, column: l.line_position as usize, message },
        Some(l) => {
            let text = full.lines().nth(l.line_number as usize - 1).unwrap_or("").trim();
            ShaderError { line: 0, column: 0, message: format!("{message} (in engine code: `{text}`)") }
        }
        None => ShaderError { line: 0, column: 0, message },
    }
}

/// Compile a user shader to SPIR-V.
pub fn compile(user: &str) -> Result<Vec<u32>, Vec<ShaderError>> {
    let full = compose(user);
    let module = naga::front::wgsl::parse_str(&full).map_err(|e| {
        let labels: Vec<String> = e.labels().filter(|(_, m)| !m.is_empty()).map(|(_, m)| m.to_string()).collect();
        let message = if labels.is_empty() { e.message().to_string() } else { format!("{} ({})", e.message(), labels.join("; ")) };
        vec![error_at(user, &full, e.location(&full), message)]
    })?;
    if !module.entry_points.iter().any(|e| e.name == "fs_main") || !module.functions.iter().any(|(_, f)| f.name.as_deref() == Some("surface")) {
        return Err(vec![ShaderError { line: 0, column: 0, message: "the shader must define `fn surface(s: Surface) -> vec4<f32>`".into() }]);
    }
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::PUSH_CONSTANT)
        .validate(&module)
        .map_err(|e| {
            let mut message = e.as_inner().to_string();
            let mut source: Option<&dyn std::error::Error> = std::error::Error::source(e.as_inner());
            while let Some(s) = source {
                message.push_str(": ");
                message.push_str(&s.to_string());
                source = s.source();
            }
            vec![error_at(user, &full, e.location(&full), message)]
        })?;
    let options = naga::back::spv::Options { flags: naga::back::spv::WriterFlags::empty(), ..Default::default() };
    naga::back::spv::write_vec(&module, &info, &options, None)
        .map_err(|e| vec![ShaderError { line: 0, column: 0, message: format!("SPIR-V generation failed: {e}") }])
}

/// Errors of a user shader (empty when it compiles).
pub fn check(user: &str) -> Vec<ShaderError> {
    compile(user).err().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_compiles() {
        assert!(compile(TEMPLATE).is_ok(), "{:?}", check(TEMPLATE));
    }

    #[test]
    fn vertex_hook_is_used() {
        let src = format!("{TEMPLATE}\nfn vertex(v: Vertex) -> Vertex {{ var o = v; o.world_pos.y += sin(time()); return o; }}\n");
        assert!(compile(&src).is_ok(), "{:?}", check(&src));
    }

    #[test]
    fn errors_point_at_the_user_line() {
        let src = "fn surface(s: Surface) -> vec4<f32> {\n    return vec4<f32>(s.nope, 1.0);\n}\n";
        let e = check(src);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].line, 2, "{e:?}");
    }

    #[test]
    fn missing_surface_is_reported() {
        let e = check("fn other() {}\n");
        assert!(!e.is_empty());
    }
}
