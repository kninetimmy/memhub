use std::io;
use std::path::Path;

#[cfg(unix)]
use std::fs;

#[derive(Debug, Clone, PartialEq, Eq)]
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
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::metadata(path)?;
    Ok(FileInfo {
        identity: FileIdentity {
            first: metadata.dev(),
            second: metadata.ino(),
        },
        links: metadata.nlink(),
    })
}

#[cfg(windows)]
pub(crate) fn file_info(path: &Path) -> io::Result<FileInfo> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;

    type Handle = *mut c_void;
    const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
    const FILE_SHARE_READ: u32 = 0x0000_0001;
    const FILE_SHARE_WRITE: u32 = 0x0000_0002;
    const FILE_SHARE_DELETE: u32 = 0x0000_0004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

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
        fn CreateFileW(
            file_name: *const u16,
            desired_access: u32,
            share_mode: u32,
            security_attributes: *mut c_void,
            creation_disposition: u32,
            flags_and_attributes: u32,
            template_file: Handle,
        ) -> Handle;
        fn GetFileInformationByHandle(
            file: Handle,
            information: *mut ByHandleFileInformation,
        ) -> i32;
        fn CloseHandle(object: Handle) -> i32;
    }

    struct OwnedHandle(Handle);
    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            // SAFETY: `self.0` is the live handle returned by CreateFileW
            // and this owner closes it exactly once.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: `wide` is NUL-terminated and remains alive for the call; all
    // optional pointer arguments are null and the constants follow the
    // CreateFileW contract for opening either a file or directory metadata.
    let raw = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if raw == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let handle = OwnedHandle(raw);
    let mut information = std::mem::MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `handle` is valid and `information` points to writable storage
    // of the exact BY_HANDLE_FILE_INFORMATION layout.
    if unsafe { GetFileInformationByHandle(handle.0, information.as_mut_ptr()) } == 0 {
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
