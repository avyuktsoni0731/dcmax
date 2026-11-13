fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        // LiveKit's WebRTC static library registers NSString categories. Without
        // this flag the linker drops them and room connect aborts on macOS.
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
