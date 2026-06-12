// Link libmpv only when the `video` feature is enabled, so the wall still builds on machines
// without libmpv. The `video` binary (src/bin/video.rs) requires this feature.
fn main() {
    if std::env::var("CARGO_FEATURE_VIDEO").is_ok() {
        pkg_config::probe_library("mpv").expect("libmpv not found via pkg-config (install mpv)");
    }
}
