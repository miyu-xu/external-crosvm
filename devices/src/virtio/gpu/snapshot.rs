// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::path::Path;
use std::path::PathBuf;

use anyhow::anyhow;
use anyhow::Context;
use rutabaga_gfx::RutabagaSnapshotReader;
use rutabaga_gfx::RutabagaSnapshotWriter;
use tempfile::TempDir;
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

struct ScopedDir {
    dir: PathBuf,
}

impl ScopedDir {
    pub fn in_tempdir() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir().context("failed to create temporary directory")?;

        // Take ownership.
        let dir = dir.into_path();

        Ok(Self { dir })
    }

    pub fn in_existing(parent: &Path) -> anyhow::Result<Self> {
        let dir = parent.join("scopeddir");

        std::fs::create_dir_all(&dir).with_context(|| {
            format!(
                "failed to create scoped directory under {}",
                parent.display()
            )
        })?;

        Ok(Self { dir })
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }
}

impl Drop for ScopedDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub struct SnapshotArchiveWriter {
    dir: ScopedDir,
    writer: RutabagaSnapshotWriter,
}

impl SnapshotArchiveWriter {
    pub fn new(scratch_directory: &Option<PathBuf>) -> anyhow::Result<Self> {
        let dir = if let Some(scratch_directory) = scratch_directory {
            ScopedDir::in_existing(Path::new(scratch_directory))
        } else {
            ScopedDir::in_tempdir()
        }?;

        let writer = RutabagaSnapshotWriter::from_existing(dir.path().to_path_buf());

        Ok(Self { dir, writer })
    }

    pub fn add_namespace(&self, name: &str) -> anyhow::Result<RutabagaSnapshotWriter> {
        Ok(self.writer.add_namespace(name)?)
    }

    pub fn collect_fragments_into_archive(&self) -> anyhow::Result<AnySnapshot> {
        let mut paths_with_contents = Vec::new();

        let snapshot_root = self.dir.path();
        let snapshot_files = get_files_under(snapshot_root).with_context(|| {
            format!(
                "failed to list snapshot files under {}",
                snapshot_root.display()
            )
        })?;

        for path in snapshot_files.into_iter() {
            let contents: Vec<u8> = std::fs::read(&path)
                .with_context(|| format!("failed to read snapshot file {}", path.display()))?;

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

            paths_with_contents.push((ciborium::Value::Text(relative_path), ciborium::Value::Bytes(contents)))
        }

        let map = ciborium::Value::Map(paths_with_contents);

        AnySnapshot::to_any(map)
    }
}

pub struct SnapshotArchiveReader {
    _dir: ScopedDir,
    reader: RutabagaSnapshotReader,
}

impl SnapshotArchiveReader {
    pub fn unpack(scratch_directory: &Option<PathBuf>, mut archive: AnySnapshot) -> anyhow::Result<Self> {
        let dir = if let Some(scratch_directory) = scratch_directory {
            ScopedDir::in_existing(Path::new(scratch_directory))
        } else {
            ScopedDir::in_tempdir()
        }
        .context("failed to get directory for snapshot archive reader")?;

        let snapshot_root = dir.path().to_path_buf();

        let paths_to_contents_cbor: ciborium::Value =  AnySnapshot::from_any(archive)
            .context("failed to get cbor from snapshot")?;
        let paths_to_contents_map =  paths_to_contents_cbor
            .as_map()
            .context("recevied non map object from snapshot?")?;

        for (path_cbor, contents_cbor) in paths_to_contents_map.into_iter() {
            let path = path_cbor
                .as_text()
                .context("non string path found in snapshot object?")?;
            let path = snapshot_root.join(path);
            let path_directory = path.parent().with_context(|| {
                format!("failed to get parent directory for {}", path.display())
            })?;
            std::fs::create_dir_all(path_directory)
                .with_context(|| format!("failed to create directories for {}", path.display()))?;

            let contents = contents_cbor
                .as_bytes()
                .ok_or(anyhow!("unexpected non-string json object"))?;

            std::fs::write(&path, contents)
                .with_context(|| format!("failed to unpack snapshot to {}", path.display()))?;
        }

        Ok(Self {
            _dir: dir,
            reader: RutabagaSnapshotReader::new(snapshot_root)?,
        })
    }

    pub fn get_namespace(&self, name: &str) -> anyhow::Result<RutabagaSnapshotReader> {
        Ok(self.reader.get_namespace(name)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_and_unpack() {
        let archive: serde_json::Value = {
            let archive_writer = SnapshotArchiveWriter::new(&None).unwrap();

            let outer_writer = archive_writer.add_namespace("outer").unwrap();
            let outer_value = serde_json::Value::String("outer_value".to_string());
            outer_writer
                .add_fragment("outer_file", &outer_value)
                .unwrap();

            let inner1_writer = outer_writer.add_namespace("inner1").unwrap();
            let inner1_value1 = serde_json::Value::String("inner1_value1".to_string());
            inner1_writer
                .add_fragment("inner1_file1", &inner1_value1)
                .unwrap();
            let inner1_value2 = serde_json::Value::String("inner1_value2".to_string());
            inner1_writer
                .add_fragment("inner1_file2", &inner1_value2)
                .unwrap();

            archive_writer.collect_fragments_into_archive().unwrap()
        };

        let archive_reader = SnapshotArchiveReader::unpack(&None, archive).unwrap();

        let outer_reader = archive_reader.get_namespace("outer").unwrap();
        let outer_value: serde_json::Value = outer_reader.get_fragment("outer_file").unwrap();
        assert_eq!(outer_value.as_str(), Some("outer_value"));

        let inner1_reader = outer_reader.get_namespace("inner1").unwrap();
        let inner1_value1: serde_json::Value = inner1_reader.get_fragment("inner1_file1").unwrap();
        assert_eq!(inner1_value1.as_str(), Some("inner1_value1"));
        let inner1_value2: serde_json::Value = inner1_reader.get_fragment("inner1_file2").unwrap();
        assert_eq!(inner1_value2.as_str(), Some("inner1_value2"));
    }
}
