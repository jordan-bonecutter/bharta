//! The portable executable contains a glibc GPU host, built against an old
//! baseline with private Zig. Desktop driver libraries use the glibc ABI and
//! cannot be loaded into a static musl process. Execute our own embedded host
//! from an anonymous file; retain the static renderer if its loader is missing.
#[cfg(target_env = "musl")]
mod embedded {
    include!(concat!(env!("OUT_DIR"), "/gpu_payload.rs"));
    use std::{
        io::Write,
        os::{fd::FromRawFd, unix::process::CommandExt},
        process::{Command, Stdio},
    };
    pub fn run() -> std::io::Result<()> {
        if PAYLOAD.is_empty() || std::env::var("BHARTA_RENDERER").as_deref() == Ok("software") {
            return Ok(());
        }
        // SAFETY: constant NUL-terminated name; memfd returns a new owned fd.
        let fd = unsafe { libc::memfd_create(c"bharta-gpu".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: ownership of the new descriptor is transferred exactly once.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(PAYLOAD)?;
        let path = format!("/proc/self/fd/{fd}");
        // A missing/incompatible glibc loader must never prevent static startup.
        if !Command::new(&path)
            .arg("--gpu-host-check")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
        {
            return Ok(());
        }
        Err(Command::new(path).args(std::env::args_os().skip(1)).exec())
    }
}
pub fn run() {
    #[cfg(target_env = "musl")]
    if let Err(error) = embedded::run() {
        eprintln!("bharta: embedded GPU host unavailable, using software: {error}");
    }
}

pub fn available() -> bool {
    #[cfg(not(target_env = "musl"))]
    {
        // SAFETY: constant library name; the handle is checked and closed once.
        let library = unsafe {
            libc::dlopen(
                c"libwayland-client.so.0".as_ptr(),
                libc::RTLD_NOW | libc::RTLD_LOCAL,
            )
        };
        if library.is_null() {
            return false;
        }
        unsafe {
            libc::dlclose(library);
        }
        true
    }
    #[cfg(target_env = "musl")]
    {
        false
    }
}
