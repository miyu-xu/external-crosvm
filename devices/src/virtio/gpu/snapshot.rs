// Copyright 2025 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::BTreeMap as Map;
use std::fs::File;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;
use serde::Serialize;
use ::snapshot::AnySnapshot;

fn get_files_recursively(directory: &Path, paths: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if directory.is_dir() {
        for entry in std::fs::read_dir(directory)? {
            let entry = entry?;
            let entry_path = entry.path();
            if entry_path.is_dir() {
                get_files_recursively(&entry_path, paths)?;
            } else {
                paths.push(entry_path.to_path_buf().clone());
            }
        }
    }
    Ok(())
}

fn get_files_under(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    get_files_recursively(directory, &mut paths)?;
    Ok(paths)
}

#[derive(Serialize, Deserialize)]
pub struct FilesSnapshot {
    files: Map<String, Vec<u8>>,
}

pub fn pack_directory_to_snapshot(directory: &Path) -> anyhow::Result<FilesSnapshot> {
    let directory_files = get_files_under(directory)
        .with_context(|| format!("failed to list snapshot files under {}", directory.display()))?;

    let mut snapshot = FilesSnapshot{
        files: Map::new(),
    };

    for path in directory_files.into_iter() {
        let contents: Vec<u8> = std::fs::read(&path)
            .with_context(|| format!("failed to read snapshot file {}", path.display()))?;

        let relative_path = path
            .strip_prefix(directory)
            .with_context(|| { format!("failed to strip {} from {}", directory.display(), path.display()) })?
            .to_string_lossy()
            .to_string();

        snapshot.files.insert(relative_path, contents);
    }

    Ok(snapshot)
}

pub fn unpack_snapshot_to_directory(directory: &Path, snapshot: FilesSnapshot) -> anyhow::Result<()> {
    for (path, contents) in snapshot.files.into_iter() {
        let path = directory.join(path);
        let path_directory = path.parent().with_context(|| {
            format!("failed to get parent directory for {}", path.display())
        })?;
        std::fs::create_dir_all(path_directory)
            .with_context(|| format!("failed to create directories for {}", path.display()))?;
        std::fs::write(&path, contents)
            .with_context(|| format!("failed to unpack snapshot to {}", path.display()))?;
    }

    Ok(())
}
