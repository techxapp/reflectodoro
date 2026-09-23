fn main() {
    #[cfg(target_os = "macos")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("ios") {
        link_ios_bridge();
    }
    tauri_build::build()
}

/// Builds `ios-bridge/` (the Swift side of ios_bridge.rs) into the Rust
/// library, exactly as `tauri_plugin::Builder::ios_path` does for published
/// plugins. It can't live in the Xcode app target instead: the plugin needs
/// `import Tauri`, and that Swift module only exists inside this Rust build.
#[cfg(target_os = "macos")]
fn link_ios_bridge() {
    use std::path::PathBuf;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let package = manifest_dir.join("ios-bridge");
    let tauri_api = PathBuf::from(
        std::env::var("DEP_TAURI_IOS_LIBRARY_PATH").expect("DEP_TAURI_IOS_LIBRARY_PATH not set by the tauri crate"),
    );
    let target = package.join(".tauri").join("tauri-api");
    let _ = std::fs::remove_dir_all(&target);
    copy_dir(&tauri_api, &target, &[".build", "Package.resolved", "Tests"]);

    println!("cargo:rerun-if-changed=ios-bridge/Package.swift");
    println!("cargo:rerun-if-changed=ios-bridge/Sources");
    tauri_utils::build::link_apple_library("reflectodoro-bridge", &package);
}

#[cfg(target_os = "macos")]
fn copy_dir(from: &std::path::Path, to: &std::path::Path, skip: &[&str]) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if skip.iter().any(|s| name == *s) {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest, &[]);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}
