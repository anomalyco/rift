use super::{Strategy, StrategyInit, create_destination};
use crate::{CopyMode, Error, InitProgress, Result, filter::CopyFilter};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::Path;
use std::ptr::{null, null_mut};
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::{ERROR_BLOCK_TOO_MANY_REFERENCES, GENERIC_READ, MAX_PATH};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_SPARSE_FILE, FILE_BASIC_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO, FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES,
    FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_STANDARD_INFO, FileBasicInfo,
    FileIdInfo, FileStandardInfo, GetFileInformationByHandleEx, GetVolumeInformationByHandleW,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DUPLICATE_EXTENTS_DATA, FILE_SET_SPARSE_BUFFER, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
    FSCTL_GET_INTEGRITY_INFORMATION, FSCTL_GET_INTEGRITY_INFORMATION_BUFFER,
    FSCTL_SET_INTEGRITY_INFORMATION, FSCTL_SET_INTEGRITY_INFORMATION_BUFFER, FSCTL_SET_SPARSE,
};

pub(super) struct RefsStrategy;

impl Strategy for RefsStrategy {
    fn copy_directory(&self, from: &Path, to: &Path, mode: CopyMode) -> Result<()> {
        let destination_parent = same_volume_parent(from, to)?;
        verify_block_cloning(destination_parent)?;
        create_destination(to)?;
        clone_tree(from, to, (mode == CopyMode::Filtered).then_some(CopyFilter))
    }

    fn initialize_directory(
        &self,
        path: &Path,
        _progress: &mut dyn FnMut(InitProgress),
    ) -> Result<StrategyInit> {
        verify_block_cloning(path)?;
        Ok(StrategyInit::AlreadyNative)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Unsupported,
}

impl EntryKind {
    fn classify(attributes: u32) -> Self {
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            Self::Unsupported
        } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            Self::Directory
        } else if attributes & FILE_ATTRIBUTE_ENCRYPTED != 0 {
            Self::Unsupported
        } else {
            Self::File
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct FileId {
    volume: u64,
    file: [u8; 16],
}

impl FileId {
    fn of(file: &File) -> io::Result<Self> {
        let id: FILE_ID_INFO = information(file)?;
        Ok(Self {
            volume: id.VolumeSerialNumber,
            file: id.FileId.Identifier,
        })
    }
}

struct SourceFile<'a> {
    path: &'a Path,
    file: File,
    basic: FILE_BASIC_INFO,
    size: u64,
}

impl<'a> SourceFile<'a> {
    fn new(path: &'a Path, file: File, basic: FILE_BASIC_INFO) -> io::Result<Self> {
        let standard: FILE_STANDARD_INFO = information(&file)?;
        Ok(Self {
            path,
            file,
            basic,
            size: standard.EndOfFile as u64,
        })
    }
}

fn same_volume_parent<'a>(from: &Path, to: &'a Path) -> Result<&'a Path> {
    let destination_parent = to
        .parent()
        .ok_or_else(|| Error::Path(format!("destination has no parent: {}", to.display())))?;
    if volume(from)? == volume(destination_parent)? {
        Ok(destination_parent)
    } else {
        Err(Error::CowUnavailable(format!(
            "Rift needs the source and the destination on the same ReFS volume, but {} and {} are on different volumes",
            from.display(),
            destination_parent.display()
        )))
    }
}

fn volume(path: &Path) -> io::Result<u64> {
    Ok(FileId::of(&open(path, FILE_READ_ATTRIBUTES)?)?.volume)
}

fn clone_tree(from: &Path, to: &Path, filter: Option<CopyFilter>) -> Result<()> {
    for entry in WalkDir::new(from)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            filter.is_none_or(|filter| {
                entry
                    .path()
                    .strip_prefix(from)
                    .map_or(true, |path| !filter.excludes(path))
            })
        })
    {
        let entry = entry?;
        let source = entry.path();
        let destination = to.join(
            source
                .strip_prefix(from)
                .map_err(|error| Error::Path(error.to_string()))?,
        );
        let file = open(source, GENERIC_READ)?;
        let basic: FILE_BASIC_INFO = information(&file)?;
        match EntryKind::classify(basic.FileAttributes) {
            EntryKind::Directory => fs::create_dir(&destination)?,
            EntryKind::File => clone_file(&SourceFile::new(source, file, basic)?, &destination)?,
            EntryKind::Unsupported => return Err(Error::UnsupportedEntry(source.to_path_buf())),
        }
    }
    Ok(())
}

fn clone_file(source: &SourceFile, destination: &Path) -> Result<()> {
    let target = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination)?;
    if source.size == 0 {
        return Ok(());
    }
    // A sparse destination keeps ReFS from allocating clusters for the length set below, and a
    // sparse source can only be cloned into a sparse destination.
    set_sparse(&target, true)
        .map_err(|error| cow_unavailable("prepare the clone of", source.path, error))?;
    let integrity = integrity(&source.file)
        .map_err(|error| cow_unavailable("read the integrity settings of", source.path, error))?;
    match_integrity(&integrity, &target);
    target.set_len(source.size)?;
    let cluster_size = u64::from(integrity.ClusterSizeInBytes);
    if !cluster_size.is_power_of_two() || cluster_size > CLONE_CHUNK {
        return Err(Error::CowUnavailable(format!(
            "{} reports an unusable cluster size of {cluster_size} bytes",
            source.path.display()
        )));
    }
    for (offset, length) in clone_regions(source.size, cluster_size) {
        duplicate_extents(&source.file, &target, offset, length)
            .map_err(|error| cow_unavailable("clone", source.path, error))?;
    }
    if source.basic.FileAttributes & FILE_ATTRIBUTE_SPARSE_FILE == 0 {
        set_sparse(&target, false)?;
    }
    Ok(())
}

// Each cloned region must stay under 4 GiB:
// https://learn.microsoft.com/windows-server/storage/refs/block-cloning
const CLONE_CHUNK: u64 = 1 << 30;

// ReFS clones whole clusters, so the last region runs past the end of file to the next cluster
// boundary. The destination keeps the exact length set before cloning.
fn clone_regions(size: u64, cluster_size: u64) -> impl Iterator<Item = (u64, u64)> {
    let end = size.div_ceil(cluster_size) * cluster_size;
    (0..end)
        .step_by(CLONE_CHUNK as usize)
        .map(move |offset| (offset, CLONE_CHUNK.min(end - offset)))
}

fn duplicate_extents(source: &File, target: &File, offset: u64, length: u64) -> io::Result<()> {
    let region = DUPLICATE_EXTENTS_DATA {
        FileHandle: source.as_raw_handle(),
        SourceFileOffset: offset as i64,
        TargetFileOffset: offset as i64,
        ByteCount: length as i64,
    };
    control(target, FSCTL_DUPLICATE_EXTENTS_TO_FILE, &region, &mut ()).map(drop)
}

fn set_sparse(file: &File, sparse: bool) -> io::Result<()> {
    control(
        file,
        FSCTL_SET_SPARSE,
        &FILE_SET_SPARSE_BUFFER { SetSparse: sparse },
        &mut (),
    )
    .map(drop)
}

fn integrity(file: &File) -> io::Result<FSCTL_GET_INTEGRITY_INFORMATION_BUFFER> {
    let mut integrity = FSCTL_GET_INTEGRITY_INFORMATION_BUFFER::default();
    control(file, FSCTL_GET_INTEGRITY_INFORMATION, &(), &mut integrity)?;
    Ok(integrity)
}

// Dev Drives refuse to change integrity settings (reflink-copy#27). A mismatch that survives
// this fails the clone itself.
fn match_integrity(source: &FSCTL_GET_INTEGRITY_INFORMATION_BUFFER, target: &File) {
    let _ = integrity(target).and_then(|current| {
        if (current.ChecksumAlgorithm, current.Flags) == (source.ChecksumAlgorithm, source.Flags) {
            return Ok(());
        }
        let settings = FSCTL_SET_INTEGRITY_INFORMATION_BUFFER {
            ChecksumAlgorithm: source.ChecksumAlgorithm,
            Reserved: 0,
            Flags: source.Flags,
        };
        control(target, FSCTL_SET_INTEGRITY_INFORMATION, &settings, &mut ()).map(drop)
    });
}

fn cow_unavailable(action: &str, path: &Path, error: io::Error) -> Error {
    let reason = if error.raw_os_error() == Some(ERROR_BLOCK_TOO_MANY_REFERENCES as i32) {
        "ReFS allows at most 8,175 clones of the same data, and this file's data has reached that limit".to_owned()
    } else {
        error.to_string()
    };
    Error::CowUnavailable(format!("failed to {action} {}: {reason}", path.display()))
}

fn verify_block_cloning(directory: &Path) -> Result<()> {
    let operation_id = ulid::Ulid::new();
    let source = directory.join(format!(".rift-refs-probe-{operation_id}"));
    let clone = directory.join(format!(".rift-refs-probe-clone-{operation_id}"));
    let result = probe(&source, &clone).map_err(|error| match error {
        Error::CowUnavailable(reason) => unsupported_volume(directory, &reason),
        error => error,
    });
    let cleanup = [&source, &clone]
        .into_iter()
        .filter(|path| path.exists())
        .try_for_each(fs::remove_file);
    result.and(cleanup.map_err(Error::from))
}

// Several clusters at either ReFS cluster size (4 KiB or 64 KiB) plus a partial one. A probe that
// fits in one partial cluster proves nothing, because ReFS shares no storage for a file's tail.
const PROBE_SIZE: usize = 3 * 65_536 + 1;

fn probe(source: &Path, clone: &Path) -> Result<()> {
    let contents = (0..PROBE_SIZE)
        .map(|index| (index % 251) as u8 + 1)
        .collect::<Vec<_>>();
    fs::write(source, &contents)?;
    let file = open(source, GENERIC_READ)?;
    let basic = information(&file)?;
    let source_file = SourceFile::new(source, file, basic)?;
    clone_file(&source_file, clone)?;
    if !same_contents(&source_file.file, &File::open(clone)?)? {
        return Err(Error::CowUnavailable(
            "the probe clone differs from its source".into(),
        ));
    }
    OpenOptions::new()
        .write(true)
        .open(clone)?
        .write_all(b"rift")?;
    if fs::read(source)? != contents {
        return Err(Error::CowUnavailable(
            "writing to the probe clone changed its source".into(),
        ));
    }
    Ok(())
}

fn unsupported_volume(directory: &Path, reason: &str) -> Error {
    let filesystem = filesystem_name(directory)
        .unwrap_or_else(|error| format!("an unidentified filesystem ({error})"));
    Error::CowUnavailable(format!(
        "{} is on {filesystem}, where Rift cannot clone files ({reason}). Rift on Windows needs a ReFS volume such as a Dev Drive (https://learn.microsoft.com/windows/dev-drive/); move the project onto one",
        directory.display()
    ))
}

fn filesystem_name(path: &Path) -> io::Result<String> {
    let directory = open(path, FILE_READ_ATTRIBUTES)?;
    let mut name = [0_u16; MAX_PATH as usize + 1];
    // SAFETY: `name` is writable for its full length, and the outputs Rift does not need are
    // null with zero length.
    let succeeded = unsafe {
        GetVolumeInformationByHandleW(
            directory.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    let length = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    Ok(String::from_utf16_lossy(&name[..length]))
}

fn same_contents(mut left: &File, mut right: &File) -> io::Result<bool> {
    left.seek(SeekFrom::Start(0))?;
    right.seek(SeekFrom::Start(0))?;
    let mut left = BufReader::with_capacity(1 << 16, left);
    let mut right = BufReader::with_capacity(1 << 16, right);
    loop {
        let length = {
            let (ours, theirs) = (left.fill_buf()?, right.fill_buf()?);
            let length = ours.len().min(theirs.len());
            if length == 0 {
                return Ok(ours.is_empty() && theirs.is_empty());
            }
            if ours[..length] != theirs[..length] {
                return Ok(false);
            }
            length
        };
        left.consume(length);
        right.consume(length);
    }
}

fn open(path: &Path, access: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

trait Information: Default {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS;
}

impl Information for FILE_BASIC_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileBasicInfo;
}

impl Information for FILE_STANDARD_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileStandardInfo;
}

impl Information for FILE_ID_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileIdInfo;
}

fn information<T: Information>(file: &File) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: `T::CLASS` names the information class whose reply has `T`'s layout, and `value`
    // is writable for `size_of::<T>()` bytes.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            T::CLASS,
            (&raw mut value).cast(),
            size_of::<T>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(value)
}

/// Output buffers that `DeviceIoControl` may fill with any bytes.
trait Pod {}

impl Pod for () {}

impl Pod for [u8] {}

impl Pod for FSCTL_GET_INTEGRITY_INFORMATION_BUFFER {}

fn control<I: ?Sized, O: Pod + ?Sized>(
    file: &File,
    code: u32,
    input: &I,
    output: &mut O,
) -> io::Result<usize> {
    let input_size = size_of_val(input);
    let output_size = size_of_val(output);
    let mut returned = 0;
    // SAFETY: `input` and `output` are live for their full sizes, `O` accepts any bytes, and the
    // handle was opened without overlapped I/O, so the call finishes before it returns.
    let succeeded = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            if input_size == 0 {
                null()
            } else {
                (input as *const I).cast()
            },
            input_size as u32,
            if output_size == 0 {
                null_mut()
            } else {
                (output as *mut O).cast()
            },
            output_size as u32,
            &mut returned,
            null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(returned as usize)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_kind_follows_file_attributes() {
        assert_eq!(EntryKind::classify(0x20), EntryKind::File);
        assert_eq!(EntryKind::classify(0x21), EntryKind::File);
        assert_eq!(EntryKind::classify(0x10), EntryKind::Directory);
        assert_eq!(EntryKind::classify(0x4010), EntryKind::Directory);
        assert_eq!(EntryKind::classify(0x4020), EntryKind::Unsupported);
        assert_eq!(EntryKind::classify(0x420), EntryKind::Unsupported);
        assert_eq!(EntryKind::classify(0x410), EntryKind::Unsupported);
    }

    #[test]
    fn clone_regions_cover_whole_clusters_in_bounded_chunks() {
        assert_eq!(clone_regions(0, 4096).collect::<Vec<_>>(), []);
        assert_eq!(clone_regions(1, 4096).collect::<Vec<_>>(), [(0, 4096)]);
        assert_eq!(
            clone_regions(65_536, 65_536).collect::<Vec<_>>(),
            [(0, 65_536)]
        );
        assert_eq!(
            clone_regions(196_609, 4096).collect::<Vec<_>>(),
            [(0, 200_704)]
        );
        assert_eq!(
            clone_regions((1 << 30) + 1, 65_536).collect::<Vec<_>>(),
            [(0, 1 << 30), (1 << 30, 65_536)]
        );
    }
}
