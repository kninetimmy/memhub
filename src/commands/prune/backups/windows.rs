//! Opt-in deletion on local NTFS with transactional locking (TxF).

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Component, Path, PathBuf, Prefix};
use std::ptr::null_mut;

use super::{Backup, Group, RETAIN, at_path, changed, display_path};
use crate::commands::sync::sha256_reader;
use crate::fs_identity::{
    FileIdentity, file_info_from_handle, final_path, is_link_or_reparse_point,
};
use crate::{MemhubError, Result};

const INVALID_HANDLE: RawHandle = -1isize as RawHandle;
const SHARE_READ: u32 = 1;
const SHARE_ALL: u32 = 7; // Read, write, and delete; only for read-only probing.
const GENERIC_READ: u32 = 0x8000_0000;
const WRITE_ATTRIBUTES: u32 = 0x100;
const DELETE: u32 = 0x0001_0000;
const OPEN_EXISTING: u32 = 3;
const FILE_ATTRIBUTE_READONLY: u32 = 0x0001;
const FILE_ATTRIBUTE_ENCRYPTED: u32 = 0x4000;
pub(super) const CLEANUP_ATTRIBUTE_MASK: u32 = FILE_ATTRIBUTE_READONLY | FILE_ATTRIBUTE_ENCRYPTED;
const OPEN_REPARSE_POINT: u32 = 0x0020_0000;
const BACKUP_SEMANTICS: u32 = 0x0200_0000;
const VOLUME_NAME_GUID: u32 = 1;
const DRIVE_FIXED: u32 = 3;
const SUPPORTS_TRANSACTIONS: u32 = 0x0020_0000;
const FILE_DISPOSITION_INFO: i32 = 4;
const FILE_STREAM_INFO: i32 = 7;
const FILE_NOT_FOUND: i32 = 2;

#[repr(C)]
struct FindData {
    attributes: u32,
    times: [u32; 6],
    size_and_reserved: [u32; 4],
    name: [u16; 260],
    short_name: [u16; 14],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetDriveTypeW(root: *const u16) -> u32;
    fn GetVolumeInformationByHandleW(
        file: RawHandle,
        volume: *mut u16,
        volume_size: u32,
        serial: *mut u32,
        max_component: *mut u32,
        flags: *mut u32,
        filesystem: *mut u16,
        filesystem_size: u32,
    ) -> i32;
    fn CreateFileTransactedW(
        name: *const u16,
        access: u32,
        share: u32,
        security: *mut c_void,
        disposition: u32,
        flags: u32,
        template: RawHandle,
        transaction: RawHandle,
        miniversion: *mut u16,
        reserved: *mut c_void,
    ) -> RawHandle;
    fn GetFileInformationByHandleEx(
        file: RawHandle,
        class: i32,
        data: *mut c_void,
        size: u32,
    ) -> i32;
    fn SetFileInformationByHandle(file: RawHandle, class: i32, data: *mut c_void, size: u32)
    -> i32;
    fn FindFirstFileTransactedW(
        name: *const u16,
        info_level: i32,
        data: *mut FindData,
        search_op: i32,
        search_filter: *mut c_void,
        flags: u32,
        transaction: RawHandle,
    ) -> RawHandle;
    fn FindClose(search: RawHandle) -> i32;
}

#[link(name = "ktmw32")]
unsafe extern "system" {
    fn CreateTransaction(
        security: *mut c_void,
        uow: *mut c_void,
        options: u32,
        isolation_level: u32,
        isolation_flags: u32,
        timeout: u32,
        description: *mut u16,
    ) -> RawHandle;
    fn CommitTransaction(transaction: RawHandle) -> i32;
}

fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut units: Vec<_> = path.as_os_str().encode_wide().collect();
    if units.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"));
    }
    units.push(0);
    Ok(units)
}

fn own(raw: RawHandle) -> io::Result<OwnedHandle> {
    if raw == INVALID_HANDLE {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: both callers transfer a newly opened Win32 handle on success.
        Ok(unsafe { OwnedHandle::from_raw_handle(raw) })
    }
}

fn unsupported(path: &Path) -> MemhubError {
    MemhubError::InvalidInput(format!(
        "'{}' requires local fixed NTFS with transactional locking",
        display_path(path)
    ))
}

pub(super) fn storage_path(dir: &Path, identity: &FileIdentity) -> Result<PathBuf> {
    if !matches!(dir.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::VerbatimDisk(_)))
    {
        return Err(unsupported(dir));
    }
    let handle = directory(dir, SHARE_ALL)?;
    let physical = final_path(&handle, VOLUME_NAME_GUID).map_err(|_| unsupported(dir))?;
    let root = volume_root(&physical).ok_or_else(|| unsupported(dir))?;
    // Derive the reviewed physical path by changing only the volume prefix.
    // A handle's GUID path must not supply different directory components.
    let mut parts = dir.components();
    parts.next(); // Checked verbatim drive prefix above.
    if parts.next() != Some(Component::RootDir) || root.join(parts.as_path()) != physical {
        return Err(changed(dir));
    }
    verify_directory(&handle, dir, identity)?;
    let root = wide(&root)?;
    let mut fs_name = [0u16; 16];
    let mut flags = 0;
    // SAFETY: `root` is a NUL-terminated volume root and `handle` is live.
    // The only requested outputs have storage for their advertised sizes.
    let supported = unsafe {
        GetDriveTypeW(root.as_ptr()) == DRIVE_FIXED
            && GetVolumeInformationByHandleW(
                handle.as_raw_handle(),
                null_mut(),
                0,
                null_mut(),
                null_mut(),
                &mut flags,
                fs_name.as_mut_ptr(),
                fs_name.len() as u32,
            ) != 0
    };
    if !supported
        || flags & SUPPORTS_TRANSACTIONS == 0
        || !fs_name[..5].iter().copied().eq("NTFS\0".encode_utf16())
    {
        return Err(unsupported(dir));
    }
    Ok(physical)
}

fn volume_root(guid: &Path) -> Option<PathBuf> {
    let Component::Prefix(prefix) = guid.components().next()? else {
        return None;
    };
    if !matches!(prefix.kind(), Prefix::Verbatim(name) if name.as_encoded_bytes().starts_with(b"Volume{"))
    {
        return None;
    }
    let mut root = prefix.as_os_str().to_os_string();
    root.push("\\");
    Some(root.into())
}

fn directory(path: &Path, share: u32) -> Result<File> {
    let handle = File::options()
        // Directory-list access participates in share checks; attribute-only
        // access does not. The pin must actually exclude write/delete opens.
        .access_mode(GENERIC_READ)
        .share_mode(share)
        .custom_flags(BACKUP_SEMANTICS | OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|e| at_path(path, e))?;
    let meta = handle.metadata().map_err(|e| at_path(path, e))?;
    if !meta.is_dir() || is_link_or_reparse_point(&meta) {
        return Err(changed(path));
    }
    Ok(handle)
}

fn verify_directory(handle: &File, reviewed: &Path, identity: &FileIdentity) -> Result<()> {
    if final_path(handle, 0).map_err(|e| at_path(reviewed, e))? != reviewed
        || file_info_from_handle(handle)
            .map_err(|e| at_path(reviewed, e))?
            .identity
            != *identity
    {
        return Err(changed(reviewed));
    }
    Ok(())
}

fn pin_directory(path: &Path) -> Result<File> {
    let handle = directory(path, SHARE_READ)?;
    if final_path(&handle, VOLUME_NAME_GUID).map_err(|e| at_path(path, e))? != path {
        return Err(changed(path));
    }
    Ok(handle)
}

fn open_locked(transaction: &OwnedHandle, path: &Path, remove: bool) -> Result<File> {
    let name = wide(path)?;
    let access = GENERIC_READ | WRITE_ATTRIBUTES | if remove { DELETE } else { 0 };
    // SAFETY: `name` and transaction are live; all optional arguments are null.
    // OPEN_EXISTING plus OPEN_REPARSE_POINT does not create or follow the final entry.
    let raw = unsafe {
        CreateFileTransactedW(
            name.as_ptr(),
            access,
            0,
            null_mut(),
            OPEN_EXISTING,
            OPEN_REPARSE_POINT,
            null_mut(),
            transaction.as_raw_handle(),
            null_mut(),
            null_mut(),
        )
    };
    own(raw).map(File::from).map_err(|e| at_path(path, e))
}

fn verify(file: &File, path: &Path, expected: &Backup, remove: bool) -> Result<()> {
    let meta = file.metadata().map_err(|e| at_path(path, e))?;
    let info = file_info_from_handle(file).map_err(|e| at_path(path, e))?;
    if !meta.is_file()
        || is_link_or_reparse_point(&meta)
        || info.links != 1
        || info.identity != expected.identity
        || meta.modified().map_err(|e| at_path(path, e))? != expected.modified
        || meta.len() != expected.bytes
        || final_path(file, VOLUME_NAME_GUID).map_err(|e| at_path(path, e))? != path
        || sha256_reader(file)? != expected.hash
    {
        return Err(changed(&expected.path));
    }
    check_file(file, meta.file_attributes(), path, remove)
}

pub(super) fn check_file(file: &File, attributes: u32, path: &Path, remove: bool) -> Result<()> {
    // Neither targets nor retention witnesses can enter TxF when EFS is set.
    if attributes & FILE_ATTRIBUTE_ENCRYPTED != 0 {
        return Err(MemhubError::InvalidInput(format!(
            "backup '{}' uses NTFS encryption; refusing cleanup",
            display_path(path)
        )));
    }
    if attributes & FILE_ATTRIBUTE_READONLY != 0 {
        return Err(MemhubError::InvalidInput(format!(
            "backup '{}' is read-only; refusing cleanup",
            display_path(path)
        )));
    }
    if remove {
        verify_only_default_stream(file, path)?;
    }
    Ok(())
}

fn verify_only_default_stream(file: &File, path: &Path) -> Result<()> {
    #[repr(C)]
    struct StreamInfo {
        next: u32,
        name_bytes: u32,
        size: i64,
        allocation_size: i64,
        name: [u16; 20],
    }
    let mut info = StreamInfo {
        next: 0,
        name_bytes: 0,
        size: 0,
        allocation_size: 0,
        name: [0; 20],
    };
    // SAFETY: `info` is aligned, writable, and large enough for the default
    // stream entry. A larger list may fail, which also refuses deletion.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FILE_STREAM_INFO,
            (&mut info as *mut StreamInfo).cast(),
            size_of::<StreamInfo>() as u32,
        )
    };
    if ok == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "cannot verify that backup '{}' has no unreviewed streams; refusing cleanup: {}",
            display_path(path),
            io::Error::last_os_error()
        )));
    }
    if info.next != 0
        || info.name_bytes != 14
        || !info.name[..7].iter().copied().eq("::$DATA".encode_utf16())
    {
        return Err(MemhubError::InvalidInput(format!(
            "backup '{}' has unreviewed streams; refusing cleanup",
            display_path(path)
        )));
    }
    Ok(())
}

fn verify_unlinked(tx: &OwnedHandle, path: &Path) -> Result<()> {
    if path
        .file_name()
        .expect("entry name")
        .encode_wide()
        .any(|unit| unit == u16::from(b'*') || unit == u16::from(b'?'))
    {
        return Err(changed(path));
    }
    let name = wide(path)?;
    let mut data = std::mem::MaybeUninit::<FindData>::uninit();
    // SAFETY: the path is NUL-terminated, `data` is a writable WIN32_FIND_DATAW
    // buffer and the transaction is live. Use an exact name, not a wildcard.
    let search = unsafe {
        FindFirstFileTransactedW(
            name.as_ptr(),
            0,
            data.as_mut_ptr(),
            0,
            null_mut(),
            0,
            tx.as_raw_handle(),
        )
    };
    if search == INVALID_HANDLE {
        let err = io::Error::last_os_error();
        if err.raw_os_error() == Some(FILE_NOT_FOUND) {
            return Ok(());
        }
        return Err(at_path(path, err));
    }
    // SAFETY: the successful call returned a search handle; FindClose owns its cleanup.
    unsafe { FindClose(search) };
    Err(MemhubError::InvalidInput(format!(
        "backup '{}' is still present inside the transaction (possibly open elsewhere); refusing cleanup",
        display_path(path)
    )))
}

fn transaction(path: &Path) -> Result<OwnedHandle> {
    // SAFETY: all arguments are either null or documented default values.
    // If we return before commit, dropping the transaction rolls it back.
    own(unsafe { CreateTransaction(null_mut(), null_mut(), 0, 0, 0, 0, null_mut()) })
        .map_err(|e| at_path(path, e))
}

fn pin_reviewed_directory(
    mut path: PathBuf,
    physical: &Path,
    reviewed: &Path,
    identity: &FileIdentity,
) -> Result<Vec<File>> {
    let children = physical
        .strip_prefix(&path)
        .map_err(|_| changed(reviewed))?;
    // Pin each ancestor before opening its child. The read-only share mask
    // excludes directory opens with write-data or rename/delete access.
    let mut dirs = vec![pin_directory(&path)?];
    for component in children.components() {
        let Component::Normal(name) = component else {
            return Err(changed(reviewed));
        };
        path.push(name);
        dirs.push(pin_directory(&path)?);
    }
    // Every handle has matched its exact volume-GUID path without following
    // a reparse point. Require the pinned leaf to prove both the reviewed
    // location and identity, not just the identity of an earlier probe.
    verify_directory(
        dirs.last().expect("chain start is pinned"),
        reviewed,
        identity,
    )?;
    Ok(dirs)
}

pub(super) fn delete(group: Group) -> Result<usize> {
    if let Some(blocker) = group.cleanup_blocker {
        return Err(blocker);
    }
    let physical = storage_path(&group.directory, &group.directory_identity)?;
    // Address the verified volume by GUID, not by its mutable drive letter.
    // Keep the verified chain pinned until commit or rollback.
    let root = volume_root(&physical).ok_or_else(|| unsupported(&group.directory))?;
    let _dirs =
        pin_reviewed_directory(root, &physical, &group.directory, &group.directory_identity)?;
    let tx = transaction(&group.directory)?;
    let mut files = Vec::new();
    for (index, expected) in group.files.iter().enumerate() {
        let remove = index >= RETAIN;
        let path = physical.join(expected.path.file_name().expect("entry name"));
        // WRITE_ATTRIBUTES makes this a TxF writer, even for the keepers.
        // Ordinary share masks alone do not exclude metadata-only writers.
        let handle = open_locked(&tx, &path, remove)?;
        verify(&handle, &path, expected, remove)?;
        files.push(handle);
    }
    let count = files.len() - RETAIN;
    // There are at least 20 locked entries newer than all these targets.
    // New entries cannot lower this bound. Nothing is marked until every
    // reviewed file has been locked and verified.
    for (handle, expected) in files.drain(RETAIN..).zip(&group.files[RETAIN..]) {
        // SAFETY: FileDispositionInfo takes a one-byte BOOLEAN. The exact
        // handle verified above has DELETE access; closing it stages removal
        // in the transaction, rather than resolving a pathname again.
        let mut delete_file = 1u8;
        let ok = unsafe {
            SetFileInformationByHandle(
                handle.as_raw_handle(),
                FILE_DISPOSITION_INFO,
                (&mut delete_file as *mut u8).cast(),
                1,
            )
        };
        if ok == 0 {
            return Err(at_path(&expected.path, io::Error::last_os_error()));
        }
    }
    // A successful disposition can still be waiting on another handle that
    // did not participate in share checks. Verify the namespace change in
    // this transaction before commit, while rollback can still leave all
    // targets intact. A pending deletion is not a completed unlink.
    for expected in &group.files[RETAIN..] {
        let path = physical.join(expected.path.file_name().expect("entry name"));
        verify_unlinked(&tx, &path)?;
    }
    // Keep the retention witnesses and ancestor handles alive through
    // commit; only the handles marked for deletion must be closed first.
    // SAFETY: `tx` is a live transaction and has not yet been committed.
    if unsafe { CommitTransaction(tx.as_raw_handle()) } == 0 {
        return Err(at_path(&group.directory, io::Error::last_os_error()));
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{self, FileTimes};
    use std::time::{Duration, SystemTime};

    #[test]
    fn directory_pin_requires_both_reviewed_location_and_identity() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let first = fs::canonicalize(first).unwrap();
        let second = fs::canonicalize(second).unwrap();
        let first_id = crate::fs_identity::file_identity(&first).unwrap();
        let second_id = crate::fs_identity::file_identity(&second).unwrap();
        assert_ne!(first_id, second_id);
        let probe = directory(&first, SHARE_ALL).unwrap();
        let physical = final_path(&probe, VOLUME_NAME_GUID).unwrap();
        drop(probe);

        // The host volume root and system parents can have unrelated writers.
        // Exercise the same pinning loop on a chain owned by this fixture.
        let parent = physical.parent().unwrap();
        let pin = |reviewed, identity| {
            pin_reviewed_directory(parent.to_path_buf(), &physical, reviewed, identity)
        };
        let pins = pin(&first, &first_id).unwrap();
        assert_eq!(pins.len(), 2);
        for (wrong_path, wrong_id) in [(&second, &first_id), (&first, &second_id)] {
            let err = pin(wrong_path, wrong_id).unwrap_err();
            assert!(
                err.to_string().contains("changed or is not a direct path"),
                "{err}"
            );
        }
        let open_for_delete = |path: &Path| {
            File::options()
                .access_mode(DELETE)
                .share_mode(SHARE_ALL)
                .custom_flags(BACKUP_SEMANTICS)
                .open(path)
        };
        for path in [&first, first.parent().unwrap()] {
            assert_eq!(
                open_for_delete(path).unwrap_err().raw_os_error(),
                Some(32) // ERROR_SHARING_VIOLATION
            );
        }
        drop(pins);
        for path in [&first, first.parent().unwrap()] {
            open_for_delete(path).unwrap();
        }
    }

    #[test]
    fn encrypted_metadata_blocks_both_targets_and_retention_witnesses() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("backup.bak");
        fs::write(&path, "fixture").unwrap();
        let file = File::open(&path).unwrap();
        let attributes = file.metadata().unwrap().file_attributes();
        for remove in [false, true] {
            check_file(&file, attributes, &path, remove).unwrap();
            // Exercise the shared eligibility rule without changing host EFS
            // policy or generating a user encryption certificate.
            let err = check_file(&file, attributes | FILE_ATTRIBUTE_ENCRYPTED, &path, remove)
                .unwrap_err();
            assert!(err.to_string().contains("uses NTFS encryption"), "{err}");
        }
    }

    #[test]
    fn retention_lock_protects_against_metadata_only_updates() {
        let temp = tempfile::tempdir().unwrap();
        let path = fs::canonicalize(temp.path()).unwrap().join("keeper.bak");
        fs::write(&path, "disposable keeper").unwrap();
        let open_metadata_writer = || {
            File::options()
                .access_mode(WRITE_ATTRIBUTES)
                .share_mode(SHARE_ALL)
                .open(&path)
        };
        let initial = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let changed = initial + Duration::from_secs(1);
        let writer = open_metadata_writer().unwrap();
        writer
            .set_times(FileTimes::new().set_modified(initial))
            .unwrap();
        let tx = transaction(&path).unwrap();
        if let Ok(keeper) = open_locked(&tx, &path, false) {
            assert!(
                writer
                    .set_times(FileTimes::new().set_modified(changed))
                    .is_err()
            );
            assert_eq!(keeper.metadata().unwrap().modified().unwrap(), initial);
        }
        drop(writer);
        let keeper = open_locked(&tx, &path, false).unwrap();
        let update = |writer: File| writer.set_times(FileTimes::new().set_modified(changed));
        assert!(open_metadata_writer().and_then(update).is_err());
        assert_eq!(keeper.metadata().unwrap().modified().unwrap(), initial);
        drop(keeper);
        drop(tx);
        open_metadata_writer().and_then(update).unwrap();
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), changed);
    }
}
