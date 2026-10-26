//! macOS-specific functionality: system identification, volume statistics,
//! user identity, and descriptor-relative filesystem primitives.

pub mod fsops;

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// Reads a string-valued sysctl such as `machdep.cpu.brand_string`.
pub fn sysctl_string(name: &str) -> Option<String> {
    let cname = CString::new(name).ok()?;
    let mut len: libc::size_t = 0;
    // SAFETY: a null buffer asks the kernel for the required length.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` has `len` writable bytes and `len` reports its size.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(len);
    let s = CStr::from_bytes_until_nul(&buf).ok()?;
    Some(s.to_string_lossy().trim().to_string())
}

/// Reads a 32-bit integer sysctl such as `hw.optional.arm64`.
pub fn sysctl_i32(name: &str) -> Option<i32> {
    let cname = CString::new(name).ok()?;
    let mut value: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `value` is a properly sized and aligned out-parameter.
    let rc = unsafe {
        libc::sysctlbyname(
            cname.as_ptr(),
            (&mut value as *mut libc::c_int).cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && len == std::mem::size_of::<libc::c_int>()).then_some(value)
}

/// Ratio between the kernel VM page size (`vm.pagesize`) and the page size
/// this process sees (`sysconf(_SC_PAGESIZE)`). It is 1 for native processes
/// and 4 for x86_64 processes translated by Rosetta 2 on Apple Silicon.
pub fn vm_page_scale() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let process_page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    match (sysctl_i32("vm.pagesize"), u64::try_from(process_page)) {
        (Some(kernel), Ok(process)) if process > 0 => {
            let kernel = u64::try_from(kernel).unwrap_or(0);
            if kernel > process && kernel % process == 0 {
                kernel / process
            } else {
                1
            }
        }
        _ => 1,
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchInfo {
    /// Architecture this MacClean binary was compiled for.
    pub binary_arch: &'static str,
    /// Hardware architecture reported by the kernel.
    pub hardware_arch: String,
    /// True when an x86_64 binary is running under Rosetta 2 translation.
    pub rosetta_translated: Option<bool>,
    pub cpu_brand: Option<String>,
}

pub fn arch_info() -> ArchInfo {
    let hardware_arch = match sysctl_i32("hw.optional.arm64") {
        Some(1) => "arm64 (Apple Silicon)".to_string(),
        _ => sysctl_string("hw.machine").unwrap_or_else(|| "unknown".to_string()),
    };
    ArchInfo {
        binary_arch: std::env::consts::ARCH,
        hardware_arch,
        // The sysctl does not exist on Intel Macs, where translation cannot occur.
        rosetta_translated: match sysctl_i32("sysctl.proc_translated") {
            Some(v) => Some(v == 1),
            None if cfg!(target_arch = "x86_64") => Some(false),
            None => None,
        },
        cpu_brand: sysctl_string("machdep.cpu.brand_string"),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OsInfo {
    pub product_version: Option<String>,
    pub build_version: Option<String>,
    pub kernel_release: Option<String>,
}

pub fn os_info() -> OsInfo {
    OsInfo {
        product_version: sysctl_string("kern.osproductversion"),
        build_version: sysctl_string("kern.osversion"),
        kernel_release: sysctl_string("kern.osrelease"),
    }
}

/// Capacity of the volume that contains a path, as reported by `statfs(2)`.
#[derive(Debug, Clone, Serialize)]
pub struct VolumeStats {
    pub queried_path: PathBuf,
    pub mount_point: PathBuf,
    pub filesystem: String,
    pub total_bytes: u64,
    /// Bytes not marked free by the filesystem. On APFS this reflects the
    /// whole container, which is shared by several volumes.
    pub used_bytes: u64,
    /// Bytes available to an unprivileged user. Excludes APFS "purgeable"
    /// space, so Finder may show a larger number.
    pub available_bytes: u64,
}

pub fn volume_stats(path: &Path) -> io::Result<VolumeStats> {
    let cpath = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))?;
    // SAFETY: `statfs` is plain old data; zeroed is a valid initial state.
    let mut st: libc::statfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and out-parameter.
    if unsafe { libc::statfs(cpath.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let bsize = u64::from(st.f_bsize);
    // SAFETY: the kernel NUL-terminates these fixed-size arrays.
    let fstype = unsafe { CStr::from_ptr(st.f_fstypename.as_ptr()) };
    let mount = unsafe { CStr::from_ptr(st.f_mntonname.as_ptr()) };
    Ok(VolumeStats {
        queried_path: path.to_path_buf(),
        mount_point: PathBuf::from(OsStr::from_bytes(mount.to_bytes())),
        filesystem: fstype.to_string_lossy().into_owned(),
        total_bytes: st.f_blocks.saturating_mul(bsize),
        used_bytes: st.f_blocks.saturating_sub(st.f_bfree).saturating_mul(bsize),
        available_bytes: st.f_bavail.saturating_mul(bsize),
    })
}

pub fn current_uid() -> u32 {
    // SAFETY: getuid cannot fail.
    unsafe { libc::getuid() }
}

/// Resolves the current user's home directory: `$HOME` when it is an
/// existing absolute directory, otherwise the password database entry.
pub fn home_dir() -> Option<PathBuf> {
    if let Some(home) = std::env::var_os("HOME") {
        let p = PathBuf::from(home);
        if p.is_absolute() && p.is_dir() {
            return Some(p);
        }
    }
    let mut buf = vec![0 as libc::c_char; 4096];
    // SAFETY: zeroed passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: buffers are valid for the duration of the call.
    let rc = unsafe {
        libc::getpwuid_r(
            current_uid(),
            &mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() {
        return None;
    }
    // SAFETY: pw_dir points into `buf`, which is still alive.
    let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
    Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes())))
}

pub fn stdin_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(libc::STDIN_FILENO) == 1 }
}

pub fn stdout_is_tty() -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(libc::STDOUT_FILENO) == 1 }
}
