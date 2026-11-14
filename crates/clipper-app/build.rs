//! Embeds the clipuh icon into the Windows executable.
//!
//! GPUI loads the window class icon via `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, ..)`,
//! so the icon must be resource id 1 (also the first icon group, which Explorer picks up).
//! GPUI's own `windows-manifest` resource uses type RT_MANIFEST, so there is no conflict.

fn main() {
    #[cfg(windows)]
    embed_icon();
}

#[cfg(windows)]
fn embed_icon() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // crates/clipper-app -> workspace root; avoids canonicalize's `\?\` prefix, which rc.exe rejects.
    let root = manifest_dir.parent().and_then(|p| p.parent()).unwrap();
    let ico = root.join("assets").join("branding").join("clipuh-icon.ico");
    assert!(ico.exists(), "clipuh-icon.ico missing");
    println!("cargo:rerun-if-changed={}", ico.display());
    let ico = ico.to_string_lossy().replace(char::from(92), "/");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let rc = out_dir.join("clipuh-icon.rc");
    std::fs::write(&rc, format!("1 ICON \"{ico}\"\n")).unwrap();
    embed_resource::compile(&rc, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
