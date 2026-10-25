//! Embeds a Windows application manifest that turns on the modern (version 6)
//! common controls, so the settings window looks like the rest of Windows
//! instead of the classic grey style. Remove this file, `mousebatt.manifest`
//! and nothing else changes if it ever causes trouble: the window then simply
//! looks old-fashioned.
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=mousebatt.manifest");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_os == "windows" && target_env == "msvc" {
        let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/MANIFESTINPUT:{dir}\\mousebatt.manifest");
    }
}
