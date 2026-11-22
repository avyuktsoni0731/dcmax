use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    compile_voice_volume();
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "windows" {
        cc::Build::new()
            .cpp(true)
            .file("native/nvenc/nvenc_session.cpp")
            .include("native/nvenc")
            .flag("/EHsc")
            .flag("/std:c++17")
            .compile("nvenc_session");
        cc::Build::new()
            .cpp(true)
            .file("native/mf/mf_decode.cpp")
            .flag("/EHsc")
            .flag("/std:c++17")
            .compile("mf_decode");
        println!("cargo:rustc-link-lib=d3d11");
        println!("cargo:rustc-link-lib=dxgi");
        println!("cargo:rustc-link-lib=ole32");
        println!("cargo:rustc-link-lib=mfplat");
        println!("cargo:rustc-link-lib=mfuuid");
        println!("cargo:rerun-if-changed=native/mf/mf_decode.cpp");
        println!("cargo:rerun-if-changed=native/nvenc/nvenc_session.cpp");
        println!("cargo:rerun-if-changed=native/nvenc/nvenc_session.h");
        println!("cargo:rerun-if-changed=native/nvenc/nvEncodeAPI.h");
        return;
    }
    if target_os != "macos" {
        return;
    }
    cc::Build::new()
        .file("native/present/present_macos.m")
        .flag("-fobjc-arc")
        .compile("present_macos");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=QuartzCore");
    println!("cargo:rustc-link-lib=framework=CoreVideo");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rerun-if-changed=native/present/present_macos.m");
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

/// The other person's voice is scaled by WebRTC's track volume. That call is
/// not exposed by the Rust bindings, so a small file reaches the same object.
fn compile_voice_volume() {
    println!("cargo:rerun-if-changed=native/voice_gain.cpp");
    let build_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("OUT_DIR is not inside target/profile/build")
        .to_path_buf();
    let include = webrtc_include(&build_dir).expect("WebRTC headers were not downloaded yet");
    let mut cc = cc::Build::new();
    cc.cpp(true)
        .file("native/voice_gain.cpp")
        .include(&include)
        .include(include.join("third_party/abseil-cpp"))
        .warnings(false);
    if std::env::var("CARGO_CFG_TARGET_OS").ok().as_deref() == Some("windows") {
        cc.flag("/EHsc").flag("/std:c++20");
    } else {
        cc.flag("-std=c++20");
    }
    for (name, value) in webrtc_defines(&include) {
        match value {
            Some(value) => cc.define(&name, value.as_str()),
            None => cc.define(&name, None),
        };
    }
    cc.compile("voice_gain");
}

fn webrtc_include(build_dir: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(build_dir).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_str().unwrap_or("");
        if !name.starts_with("scratch-") {
            continue;
        }
        let livekit = entry.path().join("out/livekit_webrtc/livekit");
        let Ok(bundles) = fs::read_dir(&livekit) else {
            continue;
        };
        for bundle in bundles.flatten() {
            let direct = bundle.path().join("include/api/media_stream_interface.h");
            if direct.is_file() {
                return Some(bundle.path().join("include"));
            }
            let Ok(children) = fs::read_dir(bundle.path()) else {
                continue;
            };
            for child in children.flatten() {
                let header = child.path().join("include/api/media_stream_interface.h");
                if header.is_file() {
                    return Some(child.path().join("include"));
                }
            }
        }
    }
    None
}

fn webrtc_defines(include: &Path) -> Vec<(String, Option<String>)> {
    let Some(root) = include.parent() else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(root.join("webrtc.ninja")) else {
        return Vec::new();
    };
    let Some(line) = text.lines().next() else {
        return Vec::new();
    };
    let mut defines = Vec::new();
    for token in line.split_whitespace() {
        let Some(body) = token.strip_prefix("-D") else {
            continue;
        };
        if body.contains('"') || body.contains('\\') {
            continue;
        }
        match body.split_once('=') {
            Some((name, value)) if !name.is_empty() => defines.push((name.to_string(), Some(value.to_string()))),
            None if !body.is_empty() => defines.push((body.to_string(), None)),
            _ => {}
        }
    }
    defines
}
