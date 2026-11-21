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

/// Open after the caller checks the exact path and its ancestors. On Unix,
/// verify against the prior non-following metadata before reading any bytes.
pub(crate) fn open_direct_file(path: &Path, expected: &fs::Metadata) -> io::Result<fs::File> {
    let mut options = fs::File::options();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    let actual = file.metadata()?;
    if !expected.is_file() || !actual.is_file() || is_link_or_reparse_point(&actual) {
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
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    use std::os::windows::fs::OpenOptionsExt;

    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

    let file = fs::File::options()
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    file_info_from_handle(&file)
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
