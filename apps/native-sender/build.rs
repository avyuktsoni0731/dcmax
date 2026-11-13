use std::path::Path;
use std::process::Command;

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    // LiveKit's WebRTC static library registers NSString categories. Without
    // this flag the linker drops them and room connect aborts on macOS.
    println!("cargo:rustc-link-arg=-ObjC");
    // ScreenCaptureKit's Swift bridge auto-links these. A dependency build
    // script cannot pass the search path through to this binary.
    if let Some(swift_libs) = swift_static_lib_dir() {
        println!("cargo:rustc-link-search=native={swift_libs}");
        for library in [
            "swiftCompatibility56",
            "swiftCompatibilityConcurrency",
            "swiftCompatibilityPacks",
        ] {
            println!("cargo:rustc-link-lib=static={library}");
        }
    }
    for runtime in swift_runtime_dirs() {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{runtime}");
    }
}

fn swift_static_lib_dir() -> Option<String> {
    let output = Command::new("xcode-select").arg("-p").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?;
    let root = root.trim();
    let candidates = [
        format!("{root}/usr/lib/swift/macosx"),
        format!("{root}/usr/lib/swift-5.5/macosx"),
        format!("{root}/Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx"),
    ];
    candidates
        .into_iter()
        .find(|path| Path::new(path).join("libswiftCompatibility56.a").is_file())
}

fn swift_runtime_dirs() -> Vec<String> {
    let mut dirs = vec!["/usr/lib/swift".to_string()];
    if let Ok(output) = Command::new("xcode-select").arg("-p").output() {
        if output.status.success() {
            if let Ok(root) = String::from_utf8(output.stdout) {
                let root = root.trim();
                for suffix in [
                    "usr/lib/swift/macosx",
                    "usr/lib/swift-5.5/macosx",
                    "Toolchains/XcodeDefault.xctoolchain/usr/lib/swift/macosx",
                ] {
                    let path = format!("{root}/{suffix}");
                    if Path::new(&path)
                        .join("libswift_Concurrency.dylib")
                        .is_file()
                    {
                        dirs.push(path);
                    }
                }
            }
        }
    }
    dirs
}
