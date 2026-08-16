// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

fn main() {
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        // Build r8brain in the same GNU toolchain as crosvm. The old downloaded
        // DLL was a debug MSVC binary and pulled debug CRT libraries into the
        // published runtime. r8brain's C ABI wrapper supports an empty
        // R8BSRC_DECL specifically for static object builds.
        cc::Build::new()
            .cpp(true)
            .file("third_party/r8brain/DLL/r8bsrc.cpp")
            .include("third_party/r8brain")
            .define("R8BSRC_DECL", Some(""))
            .warnings(false)
            .compile("r8brain");

        println!("cargo:rerun-if-changed=third_party/r8brain/DLL/r8bsrc.cpp");
        println!("cargo:rerun-if-changed=third_party/r8brain/DLL/r8bsrc.h");
        println!("cargo:rerun-if-changed=third_party/r8brain");
    }
}
