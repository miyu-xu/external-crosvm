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

struct ScopedDir {
    dir: PathBuf,
}

impl ScopedDir {
    pub fn in_tempdir() -> anyhow::Result<Self> {
        let dir = tempfile::tempdir()
            .context("failed to create temporary directory")?;

        // Take ownership.
        let dir = dir.into_path();

        Ok(Self { dir })
    }

    pub fn in_existing(parent: &Path) -> anyhow::Result<Self> {
        let dir = parent.join("scopeddir");

        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create scoped directory under {}", parent.display()))?;

        Ok(Self{ dir })
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
    pub fn new(scratch_directory: &Option<String>) -> anyhow::Result<Self> {
        let dir =
            if let Some(scratch_directory) = scratch_directory {
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

    pub fn collect_fragments_into_archive(&self) -> anyhow::Result<serde_json::Value> {
        let mut map = serde_json::Map::new();

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

pub struct SnapshotArchiveReader {
    dir: ScopedDir,
    reader: RutabagaSnapshotReader,
}

impl SnapshotArchiveReader {
    pub fn unpack(scratch_directory: &Option<String>, mut archive: serde_json::Value) -> anyhow::Result<Self> {
        let dir =
            if let Some(scratch_directory) = scratch_directory {
                ScopedDir::in_existing(Path::new(scratch_directory))
            } else {
                ScopedDir::in_tempdir()
            }
            .context("failed to get directory for snapshot archive reader")?;

        let snapshot_root = dir.path().to_path_buf();

        for (path, contents_json) in archive
            .as_object_mut()
            .ok_or(anyhow!("received non object for snapshot archive reader"))?
            .into_iter()
        {
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
            dir,
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
            let archive_writer = SnapshotArchiveWriter::new().unwrap();

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

        let archive_reader = SnapshotArchiveReader::unpack(archive).unwrap();

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
