// Copyright 2024 The ChromiumOS Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::fmt::Debug;
use std::fmt::Formatter;
use std::fs::File;
use std::io::BufReader;
use std::io::BufWriter;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use crate::RutabagaError;
use crate::RutabagaResult;

pub struct RutabagaSnapshotWriter {
    dir: PathBuf,
}

impl RutabagaSnapshotWriter {
    pub fn new(directory: PathBuf) -> RutabagaResult<Self> {
        std::fs::create_dir(&directory).map_err(RutabagaError::IoError)?;
        Ok(Self {
            dir: directory,
        })
    }

    pub fn from_existing(directory: PathBuf) -> Self {
        Self {
            dir: directory,
        }
    }

    pub fn get_path(&self) -> PathBuf {
        self.dir.clone()
    }

    fn get_file(&self, name: &str) -> RutabagaResult<File> {
        let path = self.dir.join(name);
        File::options()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(RutabagaError::IoError)
    }

    pub fn add_namespace(&self, name: &str) -> RutabagaResult<Self> {
        let directory = self.dir.join(name);
        Self::new(directory)
    }

    pub fn add_fragment<T: serde::Serialize>(&self, name: &str, t: &T) -> RutabagaResult<()> {
        let mut w = BufWriter::new(self.get_file(name)?);
        serde_json::to_writer(&mut w, t)
            .map_err(|e| RutabagaError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?;
        w.flush()?;
        Ok(())
    }
}

pub struct RutabagaSnapshotReader {
    dir: PathBuf,
}

impl RutabagaSnapshotReader {
    pub fn new(directory: PathBuf) -> RutabagaResult<Self> {
        if !directory.as_path().exists() {
            return Err(RutabagaError::IoError(std::io::Error::new(std::io::ErrorKind::Other, format!("{} does not exist", directory.display()))));
        }

        Ok(Self {
            dir: directory,
        })
    }

    pub fn get_path(&self) -> PathBuf {
        self.dir.clone()
    }

    fn get_file(&self, name: &str) -> RutabagaResult<File> {
        let path = self.dir.join(name);
        File::open(&path).map_err(RutabagaError::IoError)
    }

    pub fn get_namespace(&self, name: &str) -> RutabagaResult<Self> {
        let directory = self.dir.join(name);
        Self::new(directory)
    }

    pub fn get_fragment<T: serde::de::DeserializeOwned>(&self, name: &str) -> RutabagaResult<T> {
        let mut r = BufReader::new(self.get_file(name)?);
        Ok(serde_json::from_reader(&mut r)
            .map_err(|e| RutabagaError::IoError(std::io::Error::new(std::io::ErrorKind::Other, e.to_string())))?
        )
    }
}
