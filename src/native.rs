//! Operating-system process identity. Never infer identity from a directory.
use crate::{Result, invalid};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Process {
    pub pid: u32,
    pub parent: u32,
    pub started_us: u64,
    pub name: String,
    pub executable: PathBuf,
    pub argv: Vec<String>,
}

impl Process {
    pub fn key(&self) -> String {
        format!("{}:{}", self.pid, self.started_us)
    }
    pub fn agent(&self) -> Option<crate::sessions::Agent> {
        let name = self.name.trim_start_matches('-');
        crate::sessions::Agent::parse(name)
            .ok()
            .or_else(|| {
                self.argv
                    .first()
                    .and_then(|s| std::path::Path::new(s).file_name())
                    .and_then(|s| s.to_str())
                    .and_then(|s| crate::sessions::Agent::parse(s).ok())
            })
            .or_else(|| {
                let path = std::env::var_os("PATH")?;
                let actual = self.executable.canonicalize().ok()?;
                [
                    crate::sessions::Agent::Agy,
                    crate::sessions::Agent::Codex,
                    crate::sessions::Agent::Claude,
                    crate::sessions::Agent::Grok,
                ]
                .into_iter()
                .find(|agent| {
                    std::env::split_paths(&path)
                        .filter(|p| p.is_absolute())
                        .any(|p| {
                            p.join(agent.name())
                                .canonicalize()
                                .is_ok_and(|p| p == actual)
                        })
                })
            })
    }
    pub fn is_shell(&self) -> bool {
        matches!(
            self.name.trim_start_matches('-'),
            "zsh" | "bash" | "fish" | "sh"
        )
    }
}

pub fn parse_argv(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() < 4 {
        return Err(invalid("truncated process arguments"));
    }
    let count = i32::from_ne_bytes(bytes[..4].try_into()?);
    if !(1..=65536).contains(&count) {
        return Err(invalid("invalid process argument count"));
    }
    let mut offset = 4;
    while offset < bytes.len() && bytes[offset] != 0 {
        offset += 1;
    }
    while offset < bytes.len() && bytes[offset] == 0 {
        offset += 1;
    }
    let mut args = Vec::new();
    for _ in 0..count {
        let end = bytes
            .get(offset..)
            .and_then(|tail| tail.iter().position(|b| *b == 0))
            .map(|length| offset + length)
            .ok_or_else(|| invalid("truncated process argument"))?;
        args.push(std::str::from_utf8(&bytes[offset..end])?.to_owned());
        offset = end + 1;
    }
    // Deliberately ignore the environment following argv: it can contain secrets.
    Ok(args)
}

#[cfg(target_os = "macos")]
pub fn process(pid: u32) -> Result<Process> {
    if pid <= 1 || pid > i32::MAX as u32 {
        return Err(invalid("invalid process ID"));
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    // SAFETY: writable buffer has exactly the size required by PROC_PIDTBSDINFO.
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size as i32,
        )
    };
    if result != size as i32 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: libproc initialized the full structure, verified above.
    let info = unsafe { info.assume_init() };
    let name: Vec<u8> = info
        .pbi_comm
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    let mut path = vec![0u8; 4096];
    // SAFETY: buffer is writable for its advertised length; libproc reads only this PID.
    let length =
        unsafe { libc::proc_pidpath(pid as i32, path.as_mut_ptr().cast(), path.len() as u32) };
    if length <= 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let end = path.iter().position(|b| *b == 0).unwrap_or(length as usize);
    let executable = PathBuf::from(std::str::from_utf8(&path[..end])?);
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as i32];
    let mut length = 0usize;
    // SAFETY: size-only sysctl query; mib is valid and no values are changed.
    let size_result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    let mut buffer = vec![
        0u8;
        if size_result == 0 && length <= 2 * 1024 * 1024 {
            length
        } else {
            0
        }
    ];
    // SAFETY: mib and byte buffer have valid lengths; no new value is supplied to sysctl.
    let result = if buffer.is_empty() {
        -1
    } else {
        unsafe {
            libc::sysctl(
                mib.as_mut_ptr(),
                3,
                buffer.as_mut_ptr().cast(),
                &mut length,
                std::ptr::null_mut(),
                0,
            )
        }
    };
    let argv = if result == 0 {
        parse_argv(&buffer[..length])?
    } else {
        Vec::new()
    };
    Ok(Process {
        pid,
        parent: info.pbi_ppid,
        started_us: info
            .pbi_start_tvsec
            .saturating_mul(1_000_000)
            .saturating_add(info.pbi_start_tvusec),
        name: String::from_utf8(name)?,
        executable,
        argv,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn process(_pid: u32) -> Result<Process> {
    Err(invalid("native process inspection requires macOS"))
}

pub fn shell_ancestor(owner: &Process) -> Result<Process> {
    let mut parent = owner.parent;
    for _ in 0..32 {
        let current = process(parent)?;
        if current.is_shell() {
            return Ok(current);
        }
        parent = current.parent;
    }
    Err(invalid("no owning interactive shell found"))
}

pub fn alive(key: &str) -> bool {
    key.split_once(':')
        .and_then(|(pid, _)| pid.parse().ok())
        .and_then(|pid| process(pid).ok())
        .is_some_and(|p| p.key() == key)
}
