//! Descriptor-relative filesystem primitives.
//!
//! Every destructive operation in MacClean goes through these functions.
//! They operate relative to an already opened directory descriptor and never
//! follow symbolic links, so a path component that is swapped for a symlink
//! after planning cannot redirect an operation outside the opened directory.

use std::ffi::{CStr, CString, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use serde::Serialize;

/// Identity of a filesystem object, stable for the object's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct FileId {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Flags that make unlinking impossible or inappropriate.
const UF_IMMUTABLE: u32 = 0x0000_0002;
const UF_APPEND: u32 = 0x0000_0004;
const SF_IMMUTABLE: u32 = 0x0002_0000;
const SF_APPEND: u32 = 0x0004_0000;

/// The subset of `struct stat` that MacClean uses.
#[derive(Debug, Clone, Copy)]
pub struct Meta {
    pub id: FileId,
    pub kind: FileKind,
    pub uid: u32,
    pub nlink: u64,
    pub logical_bytes: u64,
    /// `st_blocks * 512`: storage allocated to the object.
    pub allocated_bytes: u64,
    /// The later of modification and status-change time, in nanoseconds
    /// since the Unix epoch. Renames and metadata changes update it.
    pub changed_ns: i128,
    pub flags: u32,
}

impl Meta {
    fn from_stat(st: &libc::stat) -> Meta {
        let kind = match st.st_mode & libc::S_IFMT {
            libc::S_IFREG => FileKind::File,
            libc::S_IFDIR => FileKind::Dir,
            libc::S_IFLNK => FileKind::Symlink,
            _ => FileKind::Other,
        };
        let ns = |s: libc::time_t, n: libc::c_long| i128::from(s) * 1_000_000_000 + i128::from(n);
        Meta {
            id: FileId {
                dev: st.st_dev as u64,
                ino: st.st_ino,
            },
            kind,
            uid: st.st_uid,
            nlink: u64::from(st.st_nlink),
            logical_bytes: u64::try_from(st.st_size).unwrap_or(0),
            allocated_bytes: u64::try_from(st.st_blocks).unwrap_or(0).saturating_mul(512),
            changed_ns: ns(st.st_mtime, st.st_mtime_nsec).max(ns(st.st_ctime, st.st_ctime_nsec)),
            flags: st.st_flags,
        }
    }

    pub fn is_immutable_or_append_only(&self) -> bool {
        self.flags & (UF_IMMUTABLE | UF_APPEND | SF_IMMUTABLE | SF_APPEND) != 0
    }
}

fn cstring(name: &OsStr) -> io::Result<CString> {
    CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name contains NUL byte"))
}

/// Rejects anything that is not a single, ordinary directory entry name.
pub fn check_entry_name(name: &OsStr) -> io::Result<()> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("unsafe entry name {:?}", name),
        ));
    }
    Ok(())
}

pub fn fstat(fd: BorrowedFd<'_>) -> io::Result<Meta> {
    // SAFETY: zeroed stat is a valid out-parameter.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: fd is a valid open descriptor.
    if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&st))
}

/// `fstatat(dir, name, AT_SYMLINK_NOFOLLOW)`.
pub fn stat_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<Meta> {
    check_entry_name(name)?;
    let c = cstring(name)?;
    // SAFETY: zeroed stat is a valid out-parameter.
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: valid descriptor, C string and out-parameter.
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c.as_ptr(),
            &mut st,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Meta::from_stat(&st))
}

fn open_dir_raw(dirfd: libc::c_int, name: &CStr) -> io::Result<OwnedFd> {
    let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    // SAFETY: valid C string; the returned descriptor is owned below.
    let fd = unsafe { libc::openat(dirfd, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` was just opened and is owned by nobody else.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Opens a child directory without following a symlink at `name`.
pub fn open_dir_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<OwnedFd> {
    check_entry_name(name)?;
    open_dir_raw(dir.as_raw_fd(), &cstring(name)?)
}

/// Opens the directory at `path` without following a symlink at its last
/// component. Earlier components are resolved by the kernel; callers that
/// need more check the identity of the result with [`fstat`].
pub fn open_dir_path(path: &Path) -> io::Result<OwnedFd> {
    open_dir_raw(libc::AT_FDCWD, &cstring(path.as_os_str())?)
}

/// Opens the parent (`..`) of an open directory.
pub fn open_parent(dir: BorrowedFd<'_>) -> io::Result<OwnedFd> {
    open_dir_raw(dir.as_raw_fd(), c"..")
}

/// Opens an absolute path one component at a time with `O_NOFOLLOW`, so the
/// call fails if any component is (or has become) a symbolic link. The path
/// must therefore already be canonical.
pub fn open_dir_nofollow(path: &Path) -> io::Result<OwnedFd> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path must be absolute",
        ));
    }
    let mut current = open_dir_raw(libc::AT_FDCWD, c"/")?;
    for comp in path.components() {
        match comp {
            Component::RootDir => {}
            Component::Normal(name) => {
                current = open_dir_at(borrow(&current), name)?;
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path must not contain '.' or '..' components",
                ));
            }
        }
    }
    Ok(current)
}

pub fn borrow(fd: &OwnedFd) -> BorrowedFd<'_> {
    use std::os::fd::AsFd;
    fd.as_fd()
}

/// Lists entry names of an open directory, excluding `.` and `..`.
pub fn read_dir_names(dir: BorrowedFd<'_>) -> io::Result<Vec<OsString>> {
    // fdopendir takes ownership of its descriptor, so give it a duplicate.
    // SAFETY: F_DUPFD_CLOEXEC on a valid descriptor.
    let dup = unsafe { libc::fcntl(dir.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
    if dup < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `dup` is a valid directory descriptor we own.
    let dirp = unsafe { libc::fdopendir(dup) };
    if dirp.is_null() {
        let err = io::Error::last_os_error();
        // SAFETY: fdopendir failed, so we still own `dup`.
        unsafe { libc::close(dup) };
        return Err(err);
    }
    // The duplicate shares the file offset with `dir`; start from the top.
    // SAFETY: dirp is a valid DIR stream.
    unsafe { libc::rewinddir(dirp) };
    let mut names = Vec::new();
    let result = loop {
        // SAFETY: resetting errno distinguishes end-of-stream from failure.
        unsafe { *libc::__error() = 0 };
        // SAFETY: dirp is a valid DIR stream.
        let ent = unsafe { libc::readdir(dirp) };
        if ent.is_null() {
            let err = io::Error::last_os_error();
            break if err.raw_os_error().unwrap_or(0) == 0 {
                Ok(())
            } else {
                Err(err)
            };
        }
        // SAFETY: readdir returned a valid entry with a NUL-terminated name.
        let name = unsafe { CStr::from_ptr((*ent).d_name.as_ptr()) };
        let bytes = name.to_bytes();
        if bytes != b"." && bytes != b".." {
            names.push(OsStr::from_bytes(bytes).to_os_string());
        }
    };
    // SAFETY: closes the stream and the duplicated descriptor.
    unsafe { libc::closedir(dirp) };
    result.map(|_| names)
}

/// Removes a non-directory entry (file or the symlink itself, never its target).
pub fn unlink_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    check_entry_name(name)?;
    let c = cstring(name)?;
    // SAFETY: valid descriptor and C string.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Removes an empty directory entry.
pub fn rmdir_at(dir: BorrowedFd<'_>, name: &OsStr) -> io::Result<()> {
    check_entry_name(name)?;
    let c = cstring(name)?;
    // SAFETY: valid descriptor and C string.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), libc::AT_REMOVEDIR) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_names_reject_traversal() {
        for bad in ["", ".", "..", "a/b", "../x", "/etc"] {
            assert!(check_entry_name(OsStr::new(bad)).is_err(), "{bad:?}");
        }
        for good in ["a", "..a", "content-v2", "My App-abc"] {
            assert!(check_entry_name(OsStr::new(good)).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn open_dir_nofollow_rejects_symlinked_component() {
        let t = tempfile::tempdir().unwrap();
        let base = t.path().canonicalize().unwrap();
        std::fs::create_dir(base.join("real")).unwrap();
        std::os::unix::fs::symlink(base.join("real"), base.join("link")).unwrap();
        assert!(open_dir_nofollow(&base.join("real")).is_ok());
        assert!(open_dir_nofollow(&base.join("link")).is_err());
        assert!(open_dir_nofollow(&base.join("real/../real")).is_err());
        assert!(open_dir_nofollow(Path::new("relative")).is_err());
    }

    #[test]
    fn read_dir_names_lists_entries() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("a"), b"1").unwrap();
        std::fs::create_dir(t.path().join("b")).unwrap();
        let fd = open_dir_nofollow(&t.path().canonicalize().unwrap()).unwrap();
        let mut names = read_dir_names(borrow(&fd)).unwrap();
        names.sort();
        assert_eq!(names, vec![OsString::from("a"), OsString::from("b")]);
        // A second listing of the same descriptor still sees everything.
        assert_eq!(read_dir_names(borrow(&fd)).unwrap().len(), 2);
    }
}
