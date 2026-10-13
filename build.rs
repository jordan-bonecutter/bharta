fn main() {
    println!("cargo:rerun-if-env-changed=BHARTA_GPU_PAYLOAD");
    let source = match std::env::var("BHARTA_GPU_PAYLOAD") {
        Ok(path) if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") => {
            println!("cargo:rerun-if-changed={path}");
            format!("pub const PAYLOAD: &[u8] = include_bytes!({path:?});")
        }
        _ => "pub const PAYLOAD: &[u8] = &[];".into(),
    };
    std::fs::write(
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("gpu_payload.rs"),
        source,
    )
    .unwrap();
}
