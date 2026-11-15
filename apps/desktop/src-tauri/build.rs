fn main() {
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target == "macos" {
        cc::Build::new()
            .file("native/stage_view.m")
            .flag("-fobjc-arc")
            .compile("stage_view");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rerun-if-changed=native/stage_view.m");
        // LiveKit's WebRTC static library registers NSString categories. A
        // dependency cannot pass this flag through, and without it Join calls
        // a category that was dropped and the process aborts.
        println!("cargo:rustc-link-arg=-ObjC");
        // ScreenCaptureKit references Swift concurrency by @rpath. The working
        // MyCord bundle has this rpath; a bare debug binary does not.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build();
}
