// Copyright 2022 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    build_protos(&PathBuf::from(manifest_dir));
}

<<<<<<< HEAD   (3eb3aa Merge changes from topic "crosvm-merge-20221031")
// TODO(mikehoyle): Unify all proto-building logic across crates into a common build dependency.
fn build_protos() {
    let proto_files = vec!["protos/event_details.proto"];
    let out_dir = format!(
        "{}",
        env::var("OUT_DIR").expect("OUT_DIR env does not exist.")
    );
    fs::create_dir_all(&out_dir).unwrap();
=======
fn build_protos(manifest_dir: &PathBuf) {
    let mut event_details_path = manifest_dir.to_owned();
    event_details_path.extend(["protos", "event_details.proto"]);
>>>>>>> BRANCH (ba3e2f Add clippy tag for safety docs)

    let mut out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR env does not exist."));
    out_dir.push("metrics_protos");
    proto_build_tools::build_protos(&out_dir, &[event_details_path]);
}
