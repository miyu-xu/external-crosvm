use std::cmp::{max, min};
use std::convert::TryFrom;
use std::fs::File;
use std::io::{Error, ErrorKind, Read, Result, Seek, SeekFrom};
use std::ops::Range;
use std::os::unix::io::{AsRawFd, RawFd};

use data_model::VolatileSlice;
use protos::cdisk_spec;
use sys_util::{FileReadWriteVolatile, FileSetLen, FileSync, PunchHole, WriteZeroes};

pub trait DiskFile:
    FileSetLen + FileSync + FileReadWriteVolatile + PunchHole + Seek + WriteZeroes + Send
{
}
impl<D: FileSetLen + FileSync + PunchHole + FileReadWriteVolatile + Seek + WriteZeroes + Send>
    DiskFile for D
{
}

struct ComponentDiskPart {
    file: Box<DiskFile>,
    offset: u64,
    length: u64,
}

impl ComponentDiskPart {
    fn range(&self) -> Range<u64> {
        self.offset..(self.offset + self.length)
    }
}

pub struct CompositeDiskFile {
    component_disks: Vec<ComponentDiskPart>,
    cursor_location: u64,
    descriptor_file: File,
}

fn ranges_overlap(a: &Range<u64>, b: &Range<u64>) -> bool {
    a.contains(&b.start)
        || a.contains(&(b.end - 1))
        || b.contains(&a.start)
        || b.contains(&(a.end - 1))
}

fn range_intersection(a: &Range<u64>, b: &Range<u64>) -> Range<u64> {
    if ranges_overlap(a, b) {
        Range {
            start: max(a.start, b.start),
            end: min(a.end, b.end),
        }
    } else {
        Range { start: 0, end: 0 }
    }
}

pub static CDISK_MAGIC: &str = "composite_disk\x1d";
pub const CDISK_MAGIC_LEN: usize = 15;

impl CompositeDiskFile {
    fn new(mut disks: Vec<ComponentDiskPart>, descriptor: File) -> Result<CompositeDiskFile> {
        disks.sort_by(|d1, d2| d1.offset.cmp(&d2.offset));
        for slice in disks.windows(2) {
            if let [d1, d2] = slice {
                if d1.offset + d1.length > d2.offset {
                    let text = format!(
                        "Overlapping disk: ({} + {} > {})",
                        d1.offset, d1.length, d2.offset
                    );
                    return Err(Error::new(ErrorKind::InvalidData, text));
                } else if d1.offset + d1.length < d2.offset {
                    let text = format!(
                        "Discontingous disk: ({} + {} < {})",
                        d1.offset, d1.length, d2.offset
                    );
                    return Err(Error::new(ErrorKind::InvalidData, text));
                }
            } else {
                return Err(Error::new(
                    ErrorKind::Other,
                    "impossible: windows(2) returned bad slice",
                ));
            }
        }
        Ok(CompositeDiskFile {
            component_disks: disks,
            cursor_location: 0,
            descriptor_file: descriptor,
        })
    }

    pub fn from_file(mut file: File) -> Result<CompositeDiskFile> {
        file.seek(SeekFrom::Start(0))?;
        let mut magic_space = [0u8; CDISK_MAGIC_LEN];
        file.read_exact(&mut magic_space[..])?;
        if magic_space != CDISK_MAGIC.as_bytes() {
            return Err(Error::new(ErrorKind::InvalidData, "invalid magic header"));
        }
        let proto: cdisk_spec::CompositeDisk = protobuf::parse_from_reader(&mut file)
            .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        if proto.get_version() != 1 {
            return Err(Error::new(ErrorKind::InvalidData, "unknown version"));
        }
        let disks: Vec<ComponentDiskPart> = proto
            .get_component_disks()
            .into_iter()
            .map(|disk| {
                Ok(ComponentDiskPart {
                    // TODO(schuffelen): Support qcow members
                    file: Box::new(File::open(disk.get_file_path())?),
                    offset: disk.get_offset(),
                    length: disk.get_length(),
                })
            })
            .collect::<Result<Vec<ComponentDiskPart>>>()?;
        CompositeDiskFile::new(disks, file)
    }

    fn length(&self) -> u64 {
        self.component_disks
            .iter()
            .map(|disk| disk.offset + disk.length)
            .max()
            .unwrap_or(0)
    }

    fn disk_at_offset<'a>(&'a mut self, offset: u64) -> Result<&'a mut ComponentDiskPart> {
        self.component_disks
            .iter_mut()
            .find(|disk| disk.range().contains(&offset))
            .ok_or(Error::new(
                ErrorKind::InvalidData,
                format!("no disk at offset {}", offset),
            ))
    }

    fn disks_in_range<'a>(&'a mut self, range: &Range<u64>) -> Vec<&'a mut ComponentDiskPart> {
        self.component_disks
            .iter_mut()
            .filter(|disk| ranges_overlap(&disk.range(), range))
            .collect()
    }
}

impl FileSetLen for CompositeDiskFile {
    fn set_len(&self, _len: u64) -> Result<()> {
        Err(Error::new(ErrorKind::Other, "unsupported operation"))
    }
}

impl FileSync for CompositeDiskFile {
    fn fsync(&mut self) -> Result<()> {
        for disk in self.component_disks.iter_mut() {
            disk.file.fsync()?;
        }
        Ok(())
    }
}

impl FileReadWriteVolatile for CompositeDiskFile {
    fn read_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        let cursor_location = self.cursor_location;
        let disk = self.disk_at_offset(cursor_location)?;
        disk.file.seek(SeekFrom::Start(cursor_location - disk.offset))?;
        let subslice = if cursor_location + slice.size() > disk.offset + disk.length {
            let new_size = disk.offset + disk.length - cursor_location;
            slice
                .shorten(slice.size() - new_size)
                .map_err(|e| Error::new(ErrorKind::InvalidData, format!("{:?}", e)))?
        } else {
            slice
        };
        let result = disk.file.read_volatile(subslice);
        if let Ok(size) = result {
            self.cursor_location += size as u64;
        }
        result
    }
    fn write_volatile(&mut self, slice: VolatileSlice) -> Result<usize> {
        self.seek(SeekFrom::Start(self.cursor_location))?;
        let cursor_location = self.cursor_location;
        let disk = self.disk_at_offset(cursor_location)?;
        disk.file.seek(SeekFrom::Start(cursor_location - disk.offset))?;
        let subslice = if cursor_location + slice.size() > disk.offset + disk.length {
            let new_size = disk.offset + disk.length - cursor_location;
            slice
                .shorten(slice.size() - new_size)
                .map_err(|e| Error::new(ErrorKind::InvalidData, format!("{:?}", e)))?
        } else {
            slice
        };
        let result = disk.file.write_volatile(subslice);
        if let Ok(size) = result {
            self.cursor_location += size as u64;
        }
        result
    }
}

impl PunchHole for CompositeDiskFile {
    fn punch_hole(&mut self, offset: u64, length: u64) -> Result<()> {
        let range = offset..(offset + length);
        let disks = self.disks_in_range(&range);
        for disk in disks {
            let intersection = range_intersection(&range, &disk.range());
            let result = disk
                .file
                .punch_hole(intersection.start, intersection.end - intersection.start);
            if result.is_err() {
                return result;
            }
        }
        Ok(())
    }
}

impl Seek for CompositeDiskFile {
    fn seek(&mut self, pos: SeekFrom) -> Result<u64> {
        let cursor_location = match pos {
            SeekFrom::Start(offset) => Ok(offset),
            SeekFrom::End(offset) => u64::try_from(self.length() as i64 + offset),
            SeekFrom::Current(offset) => u64::try_from(self.cursor_location as i64 + offset),
        }
        .map_err(|e| Error::new(ErrorKind::InvalidData, e))?;
        self.cursor_location = cursor_location;
        Ok(cursor_location)
    }
}

impl WriteZeroes for CompositeDiskFile {
    fn write_zeroes(&mut self, length: usize) -> Result<usize> {
        let cursor_location = self.cursor_location;
        let disk = self.disk_at_offset(cursor_location)?;
        disk.file.seek(SeekFrom::Start(cursor_location - disk.offset))?;
        let new_length = if cursor_location + length as u64 > disk.offset + disk.length {
            (disk.offset + disk.length - cursor_location) as usize
        } else {
            length
        };
        let result = disk.file.write_zeroes(new_length);
        if let Ok(size) = result {
            self.cursor_location += size as u64;
        }
        result
    }
}

impl AsRawFd for CompositeDiskFile {
    fn as_raw_fd(&self) -> RawFd {
        self.descriptor_file.as_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use data_model::VolatileMemory;
    use sys_util::SharedMemory;

    #[test]
    fn block_overlapping_disks() {
        let descriptor: File = SharedMemory::new(None).unwrap().into();
        let file1: File = SharedMemory::new(None).unwrap().into();
        let file2: File = SharedMemory::new(None).unwrap().into();
        let disk_part1 = ComponentDiskPart {
            file: Box::new(file1),
            offset: 0,
            length: 100,
        };
        let disk_part2 = ComponentDiskPart {
            file: Box::new(file2),
            offset: 50,
            length: 100,
        };
        assert!(CompositeDiskFile::new(vec![disk_part1, disk_part2], descriptor).is_err());
    }

    #[test]
    fn block_discontiguous_disks() {
        let descriptor: File = SharedMemory::new(None).unwrap().into();
        let file1: File = SharedMemory::new(None).unwrap().into();
        let file2: File = SharedMemory::new(None).unwrap().into();
        let disk_part1 = ComponentDiskPart {
            file: Box::new(file1),
            offset: 0,
            length: 100,
        };
        let disk_part2 = ComponentDiskPart {
            file: Box::new(file2),
            offset: 150,
            length: 100,
        };
        assert!(CompositeDiskFile::new(vec![disk_part1, disk_part2], descriptor).is_err());
    }

    #[test]
    fn single_file_passthrough() {
        let descriptor: File = SharedMemory::new(None).unwrap().into();
        let file: File = SharedMemory::new(None).unwrap().into();
        let disk_part = ComponentDiskPart {
            file: Box::new(file),
            offset: 0,
            length: 100,
        };
        let mut composite = CompositeDiskFile::new(vec![disk_part], descriptor).unwrap();
        let mut input_memory = [55u8; 5];
        let input_volatile_memory = &mut input_memory[..];
        composite
            .write_all_volatile(input_volatile_memory.get_slice(0, 5).unwrap())
            .unwrap();
        composite.seek(SeekFrom::Start(0)).unwrap();
        let mut output_memory = [0u8; 5];
        let output_volatile_memory = &mut output_memory[..];
        composite
            .read_exact_volatile(output_volatile_memory.get_slice(0, 5).unwrap())
            .unwrap();
        assert_eq!(input_memory, output_memory);
    }

    #[test]
    fn triple_file_passthrough() {
        let descriptor: File = SharedMemory::new(None).unwrap().into();
        let file1: File = SharedMemory::new(None).unwrap().into();
        let file2: File = SharedMemory::new(None).unwrap().into();
        let file3: File = SharedMemory::new(None).unwrap().into();
        let disk_part1 = ComponentDiskPart {
            file: Box::new(file1),
            offset: 0,
            length: 100,
        };
        let disk_part2 = ComponentDiskPart {
            file: Box::new(file2),
            offset: 100,
            length: 100,
        };
        let disk_part3 = ComponentDiskPart {
            file: Box::new(file3),
            offset: 200,
            length: 100,
        };
        let mut composite =
            CompositeDiskFile::new(vec![disk_part1, disk_part2, disk_part3], descriptor).unwrap();
        composite.seek(SeekFrom::Start(50)).unwrap();
        let mut input_memory = [55u8; 200];
        let input_volatile_memory = &mut input_memory[..];
        composite
            .write_all_volatile(input_volatile_memory.get_slice(0, 200).unwrap())
            .unwrap();
        composite.seek(SeekFrom::Start(50)).unwrap();
        let mut output_memory = [0u8; 200];
        let output_volatile_memory = &mut output_memory[..];
        composite
            .read_exact_volatile(output_volatile_memory.get_slice(0, 200).unwrap())
            .unwrap();
        assert!(input_memory.into_iter().eq(output_memory.into_iter()));
    }
}
