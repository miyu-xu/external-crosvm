// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::Path;
use std::path::PathBuf;

use anyhow::anyhow;
use anyhow::Context;
use base64::prelude::*;
use rutabaga_gfx::RutabagaSnapshotReader;
use rutabaga_gfx::RutabagaSnapshotWriter;
use tempfile::TempDir;

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

pub struct SnapshotArchiveWriter {
    dir: Option<TempDir>,
    writer: RutabagaSnapshotWriter,
}

impl SnapshotArchiveWriter {
    pub fn new() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()
            .context("failed to create temporary directory for snapshot archive writer")?;

        let writer = RutabagaSnapshotWriter::from_existing(dir.path().to_path_buf());

        Ok(Self {
            dir: Some(dir),
            writer,
        })
    }

    pub fn add_namespace(&self, name: &str) -> anyhow::Result<RutabagaSnapshotWriter> {
        Ok(self.writer.add_namespace(name)?)
    }

    pub fn collect_fragments_into_archive(&self) -> anyhow::Result<serde_json::Value> {
        let mut map = serde_json::Map::new();

        let snapshot_root = self.dir.as_ref().unwrap().path();
        let snapshot_files = get_files_under(snapshot_root).with_context(|| {
            format!(
                "failed to list snapshot files under {}",
                snapshot_root.display()
            )
        })?;

        for path in snapshot_files.into_iter() {
            let contents: Vec<u8> = std::fs::read(&path)
                .with_context(|| format!("failed to read snapshot file {}", path.display()))?;
            let contents_str = BASE64_STANDARD.encode(contents);
            let contents_json = serde_json::Value::String(contents_str);

            let relative_path = path
                .strip_prefix(snapshot_root)
                .with_context(|| {
                    format!(
                        "failed to strip {} from {}",
                        snapshot_root.display(),
                        path.display()
                    )
                })?
                .to_string_lossy()
                .to_string();

            map.insert(relative_path, contents_json);
        }

        Ok(serde_json::Value::Object(map))
    }
}

impl Drop for SnapshotArchiveWriter {
    fn drop(&mut self) {
        self.dir.take().map(|dir| {
            let path = dir.into_path();
            base::error!(
                "Not destroying temp directory at {} for debugging.",
                path.display()
            )
        });
    }
}

pub struct SnapshotArchiveReader {
    dir: Option<TempDir>,
    reader: RutabagaSnapshotReader,
}

impl SnapshotArchiveReader {
    pub fn unpack(mut archive: serde_json::Value) -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()
            .context("failed to create temporary directory for snapshot archive reader")?;

        let snapshot_root = dir.path().to_path_buf();
        base::error!("jasonjason unpacking into {}", snapshot_root.display());

        for (path, contents_json) in archive
            .as_object_mut()
            .ok_or(anyhow!("received non object for snapshot archive reader"))?
            .into_iter()
        {
            base::error!("jasonjason unpacking path {}", path);

            let path = snapshot_root.join(path);
            let path_directory = path.parent().with_context(|| {
                format!("failed to get parent directory for {}", path.display())
            })?;
            std::fs::create_dir_all(path_directory)
                .with_context(|| format!("failed to create directories for {}", path.display()))?;

            let contents_str = contents_json
                .as_str()
                .ok_or(anyhow!("unexpected non-string json object"))?;

            let contents: Vec<u8> = BASE64_STANDARD
                .decode(contents_str)
                .context("failed to decode snapshot archive")?;

            std::fs::write(&path, contents)
                .with_context(|| format!("failed to unpack snapshot to {}", path.display()))?;
        }

        Ok(Self {
            dir: Some(dir),
            reader: RutabagaSnapshotReader::new(snapshot_root)?,
        })
    }

    pub fn get_namespace(&self, name: &str) -> anyhow::Result<RutabagaSnapshotReader> {
        Ok(self.reader.get_namespace(name)?)
    }
}

impl Drop for SnapshotArchiveReader {
    fn drop(&mut self) {
        self.dir.take().map(|dir| {
            let path = dir.into_path();
            base::error!(
                "Not destroying temp directory at {} for debugging.",
                path.display()
            )
        });
    }
}
