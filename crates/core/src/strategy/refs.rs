use super::{Strategy, StrategyInit, create_destination};
use crate::{CopyMode, Error, InitProgress, Result, filter::CopyFilter};
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Seek, SeekFrom, Write};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt, symlink_dir, symlink_file};
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use walkdir::WalkDir;
use windows_sys::Win32::Foundation::{
    ERROR_BLOCK_TOO_MANY_REFERENCES, ERROR_HANDLE_EOF, ERROR_INSUFFICIENT_BUFFER, ERROR_MORE_DATA,
    ERROR_PRIVILEGE_NOT_HELD, GENERIC_READ, GENERIC_WRITE, MAX_PATH,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_ARCHIVE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_ENCRYPTED,
    FILE_ATTRIBUTE_HIDDEN, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_NOT_CONTENT_INDEXED,
    FILE_ATTRIBUTE_READONLY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_ATTRIBUTE_SPARSE_FILE,
    FILE_ATTRIBUTE_SYSTEM, FILE_ATTRIBUTE_TAG_INFO, FILE_ATTRIBUTE_TEMPORARY, FILE_BASIC_INFO,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_ID_INFO,
    FILE_INFO_BY_HANDLE_CLASS, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FILE_STANDARD_INFO, FILE_STREAM_INFO, FILE_WRITE_ATTRIBUTES,
    FileAttributeTagInfo, FileBasicInfo, FileIdInfo, FileStandardInfo, FileStreamInfo,
    GetFileInformationByHandleEx, GetVolumeInformationByHandleW, MAXIMUM_REPARSE_DATA_BUFFER_SIZE,
    SetFileInformationByHandle,
};
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{
    DUPLICATE_EXTENTS_DATA, FILE_SET_SPARSE_BUFFER, FSCTL_DUPLICATE_EXTENTS_TO_FILE,
    FSCTL_GET_INTEGRITY_INFORMATION, FSCTL_GET_INTEGRITY_INFORMATION_BUFFER,
    FSCTL_GET_REPARSE_POINT, FSCTL_SET_INTEGRITY_INFORMATION,
    FSCTL_SET_INTEGRITY_INFORMATION_BUFFER, FSCTL_SET_REPARSE_POINT, FSCTL_SET_SPARSE,
};
use windows_sys::Win32::System::SystemServices::{
    IO_REPARSE_TAG_MOUNT_POINT, IO_REPARSE_TAG_SYMLINK,
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

    fn remove_directory(&self, path: &Path) -> Result<()> {
        match fs::remove_dir_all(path) {
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                clear_read_only(path)?;
                fs::remove_dir_all(path)?;
                Ok(())
            }
            result => Ok(result?),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Symlink { directory: bool },
    Junction,
    Unsupported,
}

impl EntryKind {
    fn of(file: &File, attributes: u32) -> io::Result<Self> {
        let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
            0
        } else {
            information::<FILE_ATTRIBUTE_TAG_INFO>(file)?.ReparseTag
        };
        Ok(Self::classify(attributes, reparse_tag))
    }

    fn classify(attributes: u32, reparse_tag: u32) -> Self {
        let directory = attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            match reparse_tag {
                IO_REPARSE_TAG_SYMLINK => Self::Symlink { directory },
                IO_REPARSE_TAG_MOUNT_POINT if directory => Self::Junction,
                _ => Self::Unsupported,
            }
        } else if directory {
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
    links: u32,
}

impl<'a> SourceFile<'a> {
    fn new(path: &'a Path, file: File, basic: FILE_BASIC_INFO) -> io::Result<Self> {
        let standard: FILE_STANDARD_INFO = information(&file)?;
        Ok(Self {
            path,
            file,
            basic,
            size: standard.EndOfFile as u64,
            links: standard.NumberOfLinks,
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
    Ok(FileId::of(&open_without_following(path, FILE_READ_ATTRIBUTES)?)?.volume)
}

fn clone_tree(from: &Path, to: &Path, filter: Option<CopyFilter>) -> Result<()> {
    let root: FILE_BASIC_INFO = information(&open_without_following(from, FILE_READ_ATTRIBUTES)?)?;
    let mut hard_links = HashMap::new();
    let mut directories = Vec::new();
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
        let file = open_without_following(source, GENERIC_READ)?;
        let basic: FILE_BASIC_INFO = information(&file)?;
        match EntryKind::of(&file, basic.FileAttributes)? {
            EntryKind::Directory => {
                fs::create_dir(&destination)?;
                directories.push((basic, destination));
            }
            EntryKind::File => {
                let source = SourceFile::new(source, file, basic)?;
                if source.links > 1 {
                    let id = FileId::of(&source.file)?;
                    if let Some(existing) = hard_links.get(&id) {
                        fs::hard_link(existing, &destination)?;
                    } else {
                        clone_file(&source, &destination)?;
                        hard_links.insert(id, destination);
                    }
                } else {
                    clone_file(&source, &destination)?;
                }
            }
            EntryKind::Symlink { directory } => {
                copy_symlink(source, &basic, &destination, directory)?;
            }
            EntryKind::Junction => copy_junction(&file, &basic, &destination)?,
            EntryKind::Unsupported => return Err(Error::UnsupportedEntry(source.to_path_buf())),
        }
    }
    for (basic, destination) in directories.into_iter().rev() {
        apply_final_metadata(
            &open_without_following(&destination, FILE_WRITE_ATTRIBUTES)?,
            &basic,
        )?;
    }
    apply_final_metadata(&open_without_following(to, FILE_WRITE_ATTRIBUTES)?, &root)?;
    Ok(())
}

enum Flush {
    Done,
    Unavailable,
}

fn clone_file(source: &SourceFile, destination: &Path) -> Result<()> {
    let flushed = flush(source.path, source.basic.FileAttributes)?;
    let target = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(destination)?;
    if source.size > 0 {
        clone_data(source, &target)?;
        match flushed {
            Flush::Done => {}
            Flush::Unavailable => {
                if !same_contents(&source.file, &target)? {
                    return Err(Error::CowUnavailable(format!(
                        "the clone of {} differs from it, so another program may be writing to it; close that program and try again",
                        source.path.display()
                    )));
                }
            }
        }
    }
    copy_streams(source, destination)?;
    apply_final_metadata(&target, &source.basic)?;
    Ok(())
}

// Block cloning shares what is on disk, so writes still in the cache came back zero-filled in
// clones (https://github.com/git-lfs/git-lfs/issues/6312, pnpm#7186). Flushing needs a writable
// handle.
fn flush(path: &Path, attributes: u32) -> Result<Flush> {
    if attributes & FILE_ATTRIBUTE_READONLY == 0 {
        return Ok(flush_writable(path)?);
    }
    let file = open_without_following(path, FILE_WRITE_ATTRIBUTES)?;
    set_attributes(&file, attributes & !FILE_ATTRIBUTE_READONLY)?;
    let flushed = flush_writable(path);
    set_attributes(&file, attributes)?;
    Ok(flushed?)
}

fn flush_writable(path: &Path) -> io::Result<Flush> {
    match OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(file) => file.sync_all().map(|()| Flush::Done),
        Err(_) => Ok(Flush::Unavailable),
    }
}

fn clone_data(source: &SourceFile, target: &File) -> Result<()> {
    // A sparse destination keeps ReFS from allocating clusters for the length set below, and a
    // sparse source can only be cloned into a sparse destination.
    set_sparse(target, true)
        .map_err(|error| cow_unavailable("prepare the clone of", source.path, error))?;
    let integrity = integrity(&source.file)
        .map_err(|error| cow_unavailable("read the integrity settings of", source.path, error))?;
    match_integrity(&integrity, target);
    target.set_len(source.size)?;
    let cluster_size = u64::from(integrity.ClusterSizeInBytes);
    if !cluster_size.is_power_of_two() || cluster_size > CLONE_CHUNK {
        return Err(Error::CowUnavailable(format!(
            "{} reports an unusable cluster size of {cluster_size} bytes",
            source.path.display()
        )));
    }
    for (offset, length) in clone_regions(source.size, cluster_size) {
        duplicate_extents(&source.file, target, offset, length)
            .map_err(|error| cow_unavailable("clone", source.path, error))?;
    }
    if source.basic.FileAttributes & FILE_ATTRIBUTE_SPARSE_FILE == 0 {
        set_sparse(target, false)?;
    }
    Ok(())
}

// Cloning into an alternate data stream crashed ReFS before a hotfix (microsoft/CopyOnWrite#24).
fn copy_streams(source: &SourceFile, destination: &Path) -> Result<()> {
    for stream in stream_names(&source.file)? {
        let mut reader = File::open(with_stream(source.path, &stream))?;
        let mut writer = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(with_stream(destination, &stream))?;
        io::copy(&mut reader, &mut writer)?;
    }
    Ok(())
}

fn with_stream(path: &Path, stream: &OsStr) -> PathBuf {
    let mut path = path.as_os_str().to_owned();
    path.push(stream);
    path.into()
}

fn stream_names(file: &File) -> io::Result<Vec<OsString>> {
    let mut buffer = vec![0_u64; 512];
    loop {
        // SAFETY: `buffer` is writable for its full length in bytes and 8-byte aligned, as
        // FILE_STREAM_INFO requires.
        let succeeded = unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                FileStreamInfo,
                buffer.as_mut_ptr().cast(),
                (buffer.len() * size_of::<u64>()) as u32,
            )
        };
        if succeeded != 0 {
            break;
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error().map(|code| code as u32) {
            Some(ERROR_MORE_DATA | ERROR_INSUFFICIENT_BUFFER) => buffer.resize(buffer.len() * 2, 0),
            Some(ERROR_HANDLE_EOF) => return Ok(Vec::new()),
            _ => return Err(error),
        }
    }
    let bytes = buffer
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .collect::<Vec<_>>();
    let mut names = Vec::new();
    let mut offset = 0;
    const NEXT_ENTRY_OFFSET: usize = std::mem::offset_of!(FILE_STREAM_INFO, NextEntryOffset);
    const STREAM_NAME_LENGTH: usize = std::mem::offset_of!(FILE_STREAM_INFO, StreamNameLength);
    const STREAM_NAME: usize = std::mem::offset_of!(FILE_STREAM_INFO, StreamName);
    let u32_at = |header: &[u8], at: usize| {
        let mut unit = [0; 4];
        unit.copy_from_slice(&header[at..at + size_of::<u32>()]);
        u32::from_le_bytes(unit) as usize
    };
    while let Some(header) = bytes.get(offset..offset + STREAM_NAME) {
        let next = u32_at(header, NEXT_ENTRY_OFFSET);
        let length = u32_at(header, STREAM_NAME_LENGTH);
        let name = bytes
            .get(offset + STREAM_NAME..offset + STREAM_NAME + length)
            .ok_or_else(|| io::Error::other("malformed alternate stream information"))?;
        let name = OsString::from_wide(
            &name
                .as_chunks::<2>()
                .0
                .iter()
                .map(|unit| u16::from_le_bytes(*unit))
                .collect::<Vec<_>>(),
        );
        if name != "::$DATA" {
            names.push(name);
        }
        if next == 0 {
            break;
        }
        offset += next;
    }
    Ok(names)
}

fn copy_symlink(
    source: &Path,
    basic: &FILE_BASIC_INFO,
    destination: &Path,
    directory: bool,
) -> Result<()> {
    let target = fs::read_link(source)?;
    let created = if directory {
        symlink_dir(&target, destination)
    } else {
        symlink_file(&target, destination)
    };
    created.map_err(|error| {
        if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD as i32) {
            Error::Io(io::Error::new(
                error.kind(),
                format!(
                    "creating the symbolic link {} needs Windows Developer Mode or administrator rights: {error}",
                    destination.display()
                ),
            ))
        } else {
            error.into()
        }
    })?;
    apply_final_metadata(
        &open_without_following(destination, FILE_WRITE_ATTRIBUTES)?,
        basic,
    )?;
    Ok(())
}

fn copy_junction(source: &File, basic: &FILE_BASIC_INFO, destination: &Path) -> Result<()> {
    let mut reparse = vec![0_u8; MAXIMUM_REPARSE_DATA_BUFFER_SIZE as usize];
    let length = control(source, FSCTL_GET_REPARSE_POINT, &(), reparse.as_mut_slice())?;
    fs::create_dir(destination)?;
    let junction = open_without_following(destination, GENERIC_WRITE)?;
    control(
        &junction,
        FSCTL_SET_REPARSE_POINT,
        &reparse[..length],
        &mut (),
    )?;
    apply_final_metadata(&junction, basic)?;
    Ok(())
}

const SETTABLE_ATTRIBUTES: u32 = FILE_ATTRIBUTE_READONLY
    | FILE_ATTRIBUTE_HIDDEN
    | FILE_ATTRIBUTE_SYSTEM
    | FILE_ATTRIBUTE_ARCHIVE
    | FILE_ATTRIBUTE_NOT_CONTENT_INDEXED
    | FILE_ATTRIBUTE_TEMPORARY;

fn apply_final_metadata(file: &File, source: &FILE_BASIC_INFO) -> io::Result<()> {
    set_basic(
        file,
        &FILE_BASIC_INFO {
            CreationTime: source.CreationTime,
            LastAccessTime: source.LastAccessTime,
            LastWriteTime: source.LastWriteTime,
            ChangeTime: 0,
            FileAttributes: settable(source.FileAttributes),
        },
    )
}

// Volumes without POSIX delete semantics, such as plain ReFS on Windows Server 2022, refuse to
// delete read-only files like Git objects.
fn clear_read_only(root: &Path) -> Result<()> {
    let mut entries = WalkDir::new(root).follow_links(false).into_iter();
    while let Some(entry) = entries.next() {
        let entry = entry?;
        let attributes = entry.metadata()?.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && entry.file_type().is_dir() {
            entries.skip_current_dir();
        }
        if attributes & FILE_ATTRIBUTE_READONLY != 0 {
            set_attributes(
                &open_without_following(entry.path(), FILE_WRITE_ATTRIBUTES)?,
                attributes & !FILE_ATTRIBUTE_READONLY,
            )?;
        }
    }
    Ok(())
}

fn set_attributes(file: &File, attributes: u32) -> io::Result<()> {
    set_basic(
        file,
        &FILE_BASIC_INFO {
            FileAttributes: settable(attributes),
            ..FILE_BASIC_INFO::default()
        },
    )
}

// Zero attributes would leave the current ones unchanged, so clearing them all takes
// FILE_ATTRIBUTE_NORMAL.
fn settable(attributes: u32) -> u32 {
    match attributes & SETTABLE_ATTRIBUTES {
        0 => FILE_ATTRIBUTE_NORMAL,
        attributes => attributes,
    }
}

fn set_basic(file: &File, basic: &FILE_BASIC_INFO) -> io::Result<()> {
    // SAFETY: `basic` is a live FILE_BASIC_INFO, the layout FileBasicInfo expects.
    let succeeded = unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileBasicInfo,
            (basic as *const FILE_BASIC_INFO).cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
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

const MAX_CLUSTER_SIZE: usize = 64 * 1024;
const PROBE_SIZE: usize = 3 * MAX_CLUSTER_SIZE + 1;

fn probe(source: &Path, clone: &Path) -> Result<()> {
    let contents = (0..PROBE_SIZE)
        .map(|index| (index % 251) as u8 + 1)
        .collect::<Vec<_>>();
    fs::write(source, &contents)?;
    let file = open_without_following(source, GENERIC_READ)?;
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
    let directory = open_without_following(path, FILE_READ_ATTRIBUTES)?;
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

fn open_without_following(path: &Path, access: u32) -> io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
}

/// # Safety
///
/// `CLASS` replies with exactly `Self`'s layout.
unsafe trait Information: Default {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS;
}

unsafe impl Information for FILE_BASIC_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileBasicInfo;
}

unsafe impl Information for FILE_STANDARD_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileStandardInfo;
}

unsafe impl Information for FILE_ID_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileIdInfo;
}

unsafe impl Information for FILE_ATTRIBUTE_TAG_INFO {
    const CLASS: FILE_INFO_BY_HANDLE_CLASS = FileAttributeTagInfo;
}

fn information<T: Information>(file: &File) -> io::Result<T> {
    let mut value = T::default();
    // SAFETY: `Information` promises `T::CLASS` replies with exactly `T`'s layout, and `value`
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

/// # Safety
///
/// Any byte pattern the OS writes is a valid value of `Self`.
unsafe trait Pod {}

unsafe impl Pod for () {}

unsafe impl Pod for [u8] {}

unsafe impl Pod for FSCTL_GET_INTEGRITY_INFORMATION_BUFFER {}

fn control<I: ?Sized, O: Pod + ?Sized>(
    file: &File,
    code: u32,
    input: &I,
    output: &mut O,
) -> io::Result<usize> {
    let input_size = size_of_val(input);
    let output_size = size_of_val(output);
    let mut returned = 0;
    // SAFETY: `input` and `output` are live for their full sizes, `Pod` promises any byte pattern
    // the OS writes is a valid `O`, and the handle was opened without overlapped I/O, so the call
    // finishes before it returns.
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
    use tempfile::{Builder, TempDir};

    fn current_volume_temp() -> TempDir {
        Builder::new()
            .prefix(".rift-core-test-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap()
    }

    #[test]
    fn refs_integration_environment_is_available() {
        if std::env::var_os("RIFT_REQUIRE_REFS_TESTS").is_none() {
            return;
        }
        let temp = current_volume_temp();
        let filesystem = filesystem_name(temp.path()).unwrap();
        assert_eq!(filesystem, "ReFS");
        verify_block_cloning(temp.path()).unwrap();
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            0,
            "the probe left files in {}",
            temp.path().display()
        );
        println!(
            "block cloning probe passed on {filesystem} at {}",
            temp.path().display()
        );
    }

    #[test]
    fn entry_kind_follows_attributes_and_reparse_tags() {
        assert_eq!(EntryKind::classify(0x20, 0), EntryKind::File);
        assert_eq!(EntryKind::classify(0x21, 0), EntryKind::File);
        assert_eq!(EntryKind::classify(0x10, 0), EntryKind::Directory);
        assert_eq!(EntryKind::classify(0x4010, 0), EntryKind::Directory);
        assert_eq!(EntryKind::classify(0x4020, 0), EntryKind::Unsupported);
        assert_eq!(
            EntryKind::classify(0x420, 0xa000_000c),
            EntryKind::Symlink { directory: false }
        );
        assert_eq!(
            EntryKind::classify(0x410, 0xa000_000c),
            EntryKind::Symlink { directory: true }
        );
        assert_eq!(EntryKind::classify(0x410, 0xa000_0003), EntryKind::Junction);
        assert_eq!(
            EntryKind::classify(0x420, 0xa000_0003),
            EntryKind::Unsupported
        );
        assert_eq!(
            EntryKind::classify(0x420, 0x9000_301a),
            EntryKind::Unsupported
        );
        assert_eq!(
            EntryKind::classify(0x420, 0x8000_001b),
            EntryKind::Unsupported
        );
        assert_eq!(
            EntryKind::classify(0x420, 0x8000_0013),
            EntryKind::Unsupported
        );
    }

    #[test]
    fn settable_attributes_never_ask_to_keep_the_current_ones() {
        assert_eq!(settable(0x8221), 0x21);
        assert_eq!(settable(0x2006), 0x2006);
        assert_eq!(settable(0x200), 0x80);
        assert_eq!(settable(0), 0x80);
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
