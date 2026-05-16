// Copyright 2020 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

static PREBUILTS_VERSION_FILENAME: &str = "prebuilts_version";
static SLIRP_LIB: &str = "libslirp.lib";
static SLIRP_DLL: &str = "libslirp-0.dll";
static GLIB_FILENAME: &str = "libglib-2.0.dll.a";

fn main() {
    // macOS: compile vmnet.framework C shim when building with the hvf feature.
    if cfg!(target_os = "macos") && cfg!(feature = "hvf") {
        let src = std::path::Path::new("src/sys/macos_hvf/vmnet_shim.c");
        if src.exists() {
            let mut cc = cc::Build::new();
            cc.file(src);
            // Use SDK framework search path when available, falling back
            // to xcrun --show-sdk-path, then /System/Library/Frameworks.
            let sdkroot = std::env::var("SDKROOT").or_else(|_| {
                std::process::Command::new("xcrun")
                    .args(["--show-sdk-path"])
                    .output()
                    .ok()
                    .and_then(|o| String::from_utf8(o.stdout).ok())
                    .map(|s| s.trim().to_string())
                    .ok_or(std::env::VarError::NotPresent)
            });
            if let Ok(ref sdk) = sdkroot {
                let fw_path = format!("{}/System/Library/Frameworks", sdk);
                cc.flag(&format!("-F{}", fw_path));
                println!("cargo:rustc-link-arg=-F{}", fw_path);
            } else {
                cc.flag("-F/System/Library/Frameworks");
            }
            cc.compile("vmnet_shim");
            // linker flags for frameworks.
            // Note: dispatch symbols come from libSystem.dylib on macOS,
            // no separate framework needed.
            println!("cargo:rustc-link-lib=framework=vmnet");
            println!("cargo:rerun-if-changed=src/sys/macos_hvf/vmnet_shim.c");
            println!("cargo:rerun-if-changed=src/sys/macos_hvf/vmnet_shim.h");
        }
    }

    // We (the Windows crosvm maintainers) submitted upstream patches to libslirp-sys so it doesn't
    // try to link directly on Windows. This is because linking on Windows tends to be specific
    // to the build system that invokes Cargo (e.g. the crosvm jCI scripts that also produce the
    // required libslirp DLL & lib). The integration here (win_slirp::main) is specific to crosvm's
    // build process.
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        let version = std::fs::read_to_string(PREBUILTS_VERSION_FILENAME)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        // TODO(b:242204245) build libslirp locally on windows from build.rs.
        let mut libs = vec![SLIRP_DLL, SLIRP_LIB];
        if std::env::var("CARGO_CFG_TARGET_ENV") == Ok("gnu".to_string()) {
            libs.push(GLIB_FILENAME);
        }
        prebuilts::download_prebuilts("libslirp", version, &libs).unwrap();
    }

    // For unix, libslirp-sys's build script will make the appropriate linking calls to pkg_config.
}
