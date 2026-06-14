// Link libmpv only when the `video` feature is enabled, so the wall still builds on machines
// without libmpv. The `video` binary (src/bin/video.rs) requires this feature.
//
// Linux/macOS: found via pkg-config. Windows (incl. Linux→Windows cross): pkg-config can't see a
// Windows mpv, so link the import library from MPV_LIB_DIR (the folder holding `libmpv.dll.a`).
// The matching `libmpv-2.dll` must sit next to the built `.exe` at runtime.
fn main() {
    if std::env::var("CARGO_FEATURE_VIDEO").is_err() {
        return;
    }
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        let dir = std::env::var("MPV_LIB_DIR").expect(
            "MPV_LIB_DIR must point to the folder containing libmpv.dll.a for the Windows build",
        );
        println!("cargo:rustc-link-search=native={dir}");
        println!("cargo:rustc-link-lib=dylib=mpv");
    } else {
        pkg_config::probe_library("mpv").expect("libmpv not found via pkg-config (install mpv)");
    }
}
