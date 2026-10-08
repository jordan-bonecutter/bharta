use anyhow::{Context, Result, ensure};
use std::ffi::{CString, c_void};
#[link(name = "fontconfig")]
unsafe extern "C" {
    fn FcConfigAppFontAddFile(config: *mut c_void, file: *const u8) -> i32;
}
pub fn register(path: &str) -> Result<String> {
    let file = CString::new(path)?;
    // Fontconfig copies the filename and owns its registered application font.
    ensure!(
        unsafe { FcConfigAppFontAddFile(std::ptr::null_mut(), file.as_ptr().cast()) } != 0,
        "Cannot register font {path}"
    );
    let output = std::process::Command::new("fc-scan")
        .args(["--format", "%{family[0]}", path])
        .output()
        .context("Read custom font family with fc-scan")?;
    ensure!(output.status.success(), "Cannot read custom font family");
    let family = String::from_utf8(output.stdout)?;
    ensure!(!family.is_empty(), "Font has no family name");
    Ok(family)
}
