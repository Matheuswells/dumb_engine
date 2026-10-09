//! Embed the compiler version so the host can refuse plugins built by a different rustc
//! (Rust has no stable ABI; host and plugin must come from the same toolchain).
fn main() {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let v = std::process::Command::new(rustc)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".into());
    println!("cargo:rustc-env=DUMB_RUSTC_VERSION={v}");
}
