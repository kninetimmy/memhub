use std::fs;
use std::io;
use std::path::Path;

pub(crate) fn is_link_or_reparse_point(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FileIdentity {
    first: u64,
    second: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileInfo {
    pub identity: FileIdentity,
    pub links: u64,
}

pub(crate) fn file_identity(path: &Path) -> io::Result<FileIdentity> {
    Ok(file_info(path)?.identity)
}

#[cfg(unix)]
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    Ok(unix_file_info(&fs::metadata(path)?))
}

#[cfg(unix)]
pub(crate) fn file_info_from_handle(file: &fs::File) -> io::Result<FileInfo> {
    Ok(unix_file_info(&file.metadata()?))
}

#[cfg(unix)]
fn unix_file_info(metadata: &fs::Metadata) -> FileInfo {
    use std::os::unix::fs::MetadataExt;

    FileInfo {
        identity: FileIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        },
        links: metadata.nlink(),
    }
}

/// Identify the entry itself after the caller checks its ancestors. Do not
/// request content access or follow the final entry on Windows.
pub(crate) fn entry_identity(path: &Path, metadata: &fs::Metadata) -> Option<FileIdentity> {
    #[cfg(unix)]
    {
        let _ = path;
        let identity = unix_file_info(metadata).identity;
        (identity.second != 0).then_some(identity)
    }
    #[cfg(windows)]
    {
        let _ = metadata;
        let file = open_metadata(path, OPEN_REPARSE_POINT).ok()?;
        if final_path(&file, 0).ok()? != path {
            return None;
        }
        file_info_from_handle(&file).ok().map(|info| info.identity)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (path, metadata);
        None
    }
}

/// Open after the caller checks the exact path and its ancestors. On Unix,
/// verify against the prior non-following metadata before reading any bytes.
pub(crate) fn open_direct_file(path: &Path, expected: &fs::Metadata) -> io::Result<fs::File> {
    if !expected.is_file() || is_link_or_reparse_point(expected) {
        return Err(io::Error::other("not a direct regular file"));
    }
    let mut options = fs::File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(OPEN_REPARSE_POINT);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A path that stops naming a regular file must not block the open
        // before we can validate the new handle.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let actual = file.metadata()?;
    if !actual.is_file() || is_link_or_reparse_point(&actual) {
        return Err(io::Error::other("not a direct regular file"));
    }
    #[cfg(windows)]
    if final_path(&file, 0)? != path {
        return Err(io::Error::other("file resolved outside its checked path"));
    }
    #[cfg(unix)]
    if unix_file_info(&actual).identity != unix_file_info(expected).identity {
        return Err(io::Error::other("file identity changed before opening"));
    }
    Ok(file)
}

#[cfg(windows)]
pub(crate) fn final_path(file: &fs::File, flags: u32) -> io::Result<std::path::PathBuf> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::{AsRawHandle, RawHandle};
    use std::ptr::null_mut;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFinalPathNameByHandleW(file: RawHandle, path: *mut u16, size: u32, flags: u32)
        -> u32;
    }

    // SAFETY: valid handle and null buffer with size zero requests the required length.
    let size = unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), null_mut(), 0, flags) };
    if size == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buf = vec![0; size as usize];
    // SAFETY: `buf` has storage for exactly `size` UTF-16 units.
    let len =
        unsafe { GetFinalPathNameByHandleW(file.as_raw_handle(), buf.as_mut_ptr(), size, flags) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    if len >= size {
        return Err(io::Error::other("handle path changed during verification"));
    }
    Ok(OsString::from_wide(&buf[..len as usize]).into())
}

#[cfg(windows)]
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;

#[cfg(windows)]
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    file_info_from_handle(&open_metadata(path, 0)?)
}

#[cfg(windows)]
fn open_metadata(path: &Path, flags: u32) -> io::Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    fs::File::options()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | flags)
        .open(path)
}

#[cfg(windows)]
pub(crate) fn file_info_from_handle(file: &fs::File) -> io::Result<FileInfo> {
    use std::os::windows::io::{AsRawHandle, RawHandle};

    #[repr(C)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[repr(C)]
    struct ByHandleFileInformation {
        attributes: u32,
        creation_time: FileTime,
        last_access_time: FileTime,
        last_write_time: FileTime,
        volume_serial_number: u32,
        file_size_high: u32,
        file_size_low: u32,
        number_of_links: u32,
        file_index_high: u32,
        file_index_low: u32,
    }

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetFileInformationByHandle(
            file: RawHandle,
            information: *mut ByHandleFileInformation,
        ) -> i32;
    }

    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: the file handle is valid and `information` points to writable storage
    // of the exact BY_HANDLE_FILE_INFORMATION layout.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: GetFileInformationByHandle succeeded and initialized every
    // field of the output structure.
    let information = unsafe { information.assume_init() };
    let index =
        (u64::from(information.file_index_high) << 32) | u64::from(information.file_index_low);
    if information.volume_serial_number == 0 && index == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "filesystem identity is unavailable",
        ));
    }
    Ok(FileInfo {
        identity: FileIdentity {
            first: u64::from(information.volume_serial_number),
            second: index,
        },
        links: u64::from(information.number_of_links),
    })
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn file_info(_path: &Path) -> io::Result<FileInfo> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem identity verification is unsupported",
    ))
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn file_info_from_handle(_file: &fs::File) -> io::Result<FileInfo> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "filesystem identity verification is unsupported",
    ))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn prune_content_open_is_nonblocking_and_reads_regular_files() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), "reviewed").unwrap();
        let meta = fs::symlink_metadata(temp.path()).unwrap();
        let mut file = open_direct_file(temp.path(), &meta).unwrap();
        // SAFETY: the descriptor is live; F_GETFL needs no additional argument.
        let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
        assert_ne!(flags, -1);
        assert_ne!(flags & libc::O_NONBLOCK, 0);
        assert_eq!(io::read_to_string(&mut file).unwrap(), "reviewed");
    }
}
