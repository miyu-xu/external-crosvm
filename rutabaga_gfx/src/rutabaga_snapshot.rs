// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::Path;
use std::path::PathBuf;

use base64::prelude::*;
use tempfile::TempDir;

use crate::RutabagaError;
use crate::RutabagaResult;

fn get_ioerror(msg: String) -> RutabagaError {
    RutabagaError::IoError(std::io::Error::new(std::io::ErrorKind::Other, msg))
}

fn get_files_recursively(directory: &Path, paths: &mut Vec<PathBuf>) -> RutabagaResult<()> {
    if directory.is_dir() {
        for entry in std::fs::read_dir(directory)
                .map_err(|e| get_ioerror(format!("failed to read directory {}: {}", directory.display(), e)))? {
            let entry = entry
                .map_err(|e| get_ioerror(format!("failed to read directory {}: entry: {}", directory.display(), e)))?;
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

pub struct RutabagaSnapshotArchiveWriter {
    dir: TempDir,
}

impl RutabagaSnapshotArchiveWriter {
    pub fn new() -> RutabagaResult<Self> {
        let dir = tempfile::tempdir()
            .map_err(|e| get_ioerror(format!("failed to create tempdir for snapshot: {}", e)))?;

        Ok(Self { dir })
    }

    pub fn path_string(&self) -> String {
        self.dir.path().to_string_lossy().to_string()
    }

    pub fn as_archive_value(&self) -> RutabagaResult<serde_json::Value> {
        let mut map = serde_json::Map::new();

        let snapshot_root = self.dir.path();
        let snapshot_files = get_files_under(snapshot_root).map_err(|e| {
            get_ioerror(format!(
                "failed to list snapshot files under {}: {}",
                snapshot_root.display(), e
            ))
        })?;

        for path in snapshot_files.into_iter() {
            let contents: Vec<u8> = std::fs::read(&path)
                .map_err(|e| get_ioerror(format!("failed to read snapshot file {}: {}", path.display(), e)))?;
            let contents_str = BASE64_STANDARD.encode(contents);
            let contents_json = serde_json::Value::String(contents_str);

            let relative_path = path
                .strip_prefix(snapshot_root)
                .map_err(|_| get_ioerror(format!("failed to strip {} from {}", snapshot_root.display(), path.display())))?
                .to_string_lossy()
                .to_string();

            map.insert(relative_path, contents_json);
        }

        Ok(serde_json::Value::Object(map))
    }
}

pub struct RutabagaSnapshotArchiveReader {
    dir: TempDir,
}

impl RutabagaSnapshotArchiveReader {
    pub fn unpack(mut archive: serde_json::Value) -> RutabagaResult<Self> {
        let dir = tempfile::tempdir()
        .map_err(|e| get_ioerror(format!("failed to create tempdir for snapshot: {}", e)))?;

        let snapshot_root = dir.path().to_path_buf();

        for (path, contents_json) in archive
            .as_object_mut()
            .ok_or(get_ioerror(format!("found non-object in snapshot")))?
            .into_iter()
        {
            let path = snapshot_root.join(path);
            let path_directory = path.parent()
                .ok_or(get_ioerror(format!("failed to get parent directory for {}", path.display())))?;

            let contents_str = contents_json
                .as_str()
                .ok_or(get_ioerror(format!("unexpected non-string json object in archive")))?;

            let contents: Vec<u8> = BASE64_STANDARD
                .decode(contents_str)
                .map_err(|e| get_ioerror(format!("failed to decode snapshot archive: {}", e)))?;

            std::fs::create_dir_all(path_directory)
                .map_err(|e| get_ioerror(format!("failed to create directories for {}", path.display())))?;

            std::fs::write(&path, contents)
                .map_err(|e| get_ioerror(format!("failed to unpack snapshot file to {}", path.display())))?;
        }

        Ok(Self {
            dir,
        })
    }

    pub fn path_string(&self) -> String {
        self.dir.path().to_string_lossy().to_string()
    }
}