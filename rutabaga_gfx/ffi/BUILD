load("@rules_license//rules:license.bzl", "license")
load("@rules_license//rules:license_kind.bzl", "license_kind")
load("@rules_rust//cargo:defs.bzl", "cargo_build_script")
load("@rules_rust//rust:defs.bzl", "rust_shared_library")

package(
    default_applicable_licenses = [":license"],
    default_visibility = ["//visibility:public"],
)

license(
    name = "license",
    license_kinds = [
        ":SPDX-license-identifier-Apache-2.0",
    ],
    license_text = "LICENSE-APACHE",
    visibility = [":__subpackages__"],
)

license_kind(
    name = "SPDX-license-identifier-Apache-2.0",
    conditions = ["notice"],
    url = "https://spdx.org/licenses/Apache-2.0.html",
)

CRATE_FEATURES = [
    "rutabaga_gfx/gfxstream",
]

rust_shared_library(
    name = "rutabaga_ffi",
    srcs = glob(["src/**/*.rs"]),
    crate_features = CRATE_FEATURES,
    edition = "2021",
    deps = [
        "//external/crosvm/rutabaga_gfx",
        "//external/qemu:libmodule-common",
        "//hardware/google/gfxstream/host:gfxstream_backend",
        "@libc",
        "@log",
        "@once_cell",
    ] + select({
        "@platforms//os:linux": ["@nix"],
        "//conditions:default": [],
    }),
)

cc_library(
    name = "c_ffi",
    hdrs = glob(["src/include/**/*.h"]),
    includes = ["src/include"],
    linkopts = select({
        "@platforms//os:windows": ["/DEFAULTLIB:Ntdll.lib"],
        "//conditions:default": [],
    }),
    deps = [
        ":rutabaga_ffi",
    ],
)

cargo_build_script(
    name = "ffi_build_script",
    srcs = glob(["**/*.rs"]),
    crate_features = CRATE_FEATURES,
    crate_root = "build.rs",
    edition = "2015",
    visibility = ["//visibility:private"],
)
