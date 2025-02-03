// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fs::File;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::rutabaga_utils::*;

fn get_files_recursively(directory: &Path, paths: &mut Vec<PathBuf>) -> RutabagaResult<()> {
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

fn get_files_under(directory: &Path) -> RutabagaResult<Vec<PathBuf>> {
    let mut paths = Vec::new();
    get_files_recursively(directory, &mut paths)?;
    Ok(paths)
}

fn pack_directory_to_snapshot(directory: PathBuf) -> RutabagaResult<ciborium::Value> {
    let directory = directory.as_path();
    let directory_files = get_files_under(directory)
        .map_err(|e| {
            RutabagaError::SnapshotError(
                format!("failed to list snapshot files under {}: {}", directory.display(), e)
            )
        })?;

    let mut paths_with_contents = Vec::new();
    for path in directory_files.into_iter() {
        let contents: Vec<u8> = std::fs::read(&path)
            .map_err(|e| {
                RutabagaError::SnapshotError(
                    format!("failed to read snapshot file {}: {}", path.display(), e)
                )
            })?;

        let relative_path = path
            .strip_prefix(directory)
            .map_err(|e| {
                RutabagaError::SnapshotError(
                    format!("failed to strip {} from {}: {}", directory.display(), path.display(), e)
                )
            })?
            .to_string_lossy()
            .to_string();

        paths_with_contents.push((ciborium::Value::Text(relative_path), ciborium::Value::Bytes(contents)))
    }

    Ok(ciborium::Value::Map(paths_with_contents))
}

fn unpack_snapshot_to_directory(directory: PathBuf, snapshot: ciborium::Value) -> RutabagaResult<()> {
    let paths_to_contents_map =  snapshot
        .as_map()
        .ok_or(RutabagaError::SnapshotError(format!("recevied non map object from snapshot?")))?;

    for (path_cbor, contents_cbor) in paths_to_contents_map.into_iter() {
        let path = path_cbor
            .as_text()
            .ok_or(RutabagaError::SnapshotError(format!("found non string path in snapshot")))?;

        let path = directory.join(path);
        let path_directory = path
            .parent()
            .ok_or_else(|| {
                RutabagaError::SnapshotError(
                    format!("failed to get parent directory for {}", path.display())
                )
            })?;

        std::fs::create_dir_all(path_directory)
            .map_err(|e| {
                RutabagaError::SnapshotError(
                    format!("failed to create directories for {}: {}", path.display(), e)
                )
            })?;

        let contents = contents_cbor
            .as_bytes()
            .ok_or(RutabagaError::SnapshotError(format!("found non bytes contents in snapshot")))?;

        std::fs::write(&path, contents)
            .map_err(|e| {
                RutabagaError::SnapshotError(
                    format!("failed to unpack snapshot to {}: {}", path.display(), e)
                )
            })?;
    }

    Ok(())
}
