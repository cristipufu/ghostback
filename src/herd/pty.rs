//! Persistent background PTY host and detachable viewport client (`ghostback pty-host` & `ghostback attach`).
//!
//! Decouples running coding-agent processes from Ghostty split surfaces so sessions can be:
//! - Hidden from view (`Hide`) while continuing to run and execute tasks in the background
//! - Re-attached to any Ghostty split in 0 ms (`SIGWINCH` redraw + rolling screen replay)
//! - Swapped or rearranged across `1x1`, `2x1`, `1x2`, `2x2`, and `3x3` grid viewports without restarting

use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::sessions::shell_quote;
use crate::{Result, files, invalid};

const MAX_REPLAY_BYTES: usize = 131_072;
const FRAME_INPUT: u8 = 0x01;
const FRAME_RESIZE: u8 = 0x02;
const VIEWPORT_CLOSE_SENTINEL: &str = "CLOSE";
const DEFAULT_PTY_COLS: u16 = 110;
const DEFAULT_PTY_ROWS: u16 = 34;
const TERM_RESET_MODES: &[u8] =
    b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l\x1b[0m";
const TERM_CLEAR_VIEWPORT: &[u8] =
    b"\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[0m\x1b[?25h\x1b[2J\x1b[H";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PtySessionMeta {
    pub pty_id: String,
    pub host_pid: u32,
    pub child_pid: u32,
    pub slave_tty: String,
    pub cwd: PathBuf,
    pub label: String,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewportBinding {
    pub slot_id: String,
    pub pty_id: String,
    pub attach_pid: u32,
    pub viewport_tty: String,
    pub updated_at: i64,
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub fn ptys_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("ptys")
}

pub fn viewports_dir(state_dir: &Path) -> PathBuf {
    state_dir.join("viewports")
}

pub fn pty_sock_path(state_dir: &Path, pty_id: &str) -> PathBuf {
    let direct = ptys_dir(state_dir).join(format!("{pty_id}.sock"));
    if direct.to_string_lossy().len() < 96 {
        return direct;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in state_dir.to_string_lossy().as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h ^= 0xff;
    for b in pty_id.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    PathBuf::from(format!("/tmp/gb-{h:016x}.sock"))
}

pub fn pty_meta_path(state_dir: &Path, pty_id: &str) -> PathBuf {
    ptys_dir(state_dir).join(format!("{pty_id}.json"))
}

pub fn viewport_meta_path(state_dir: &Path, slot_id: &str) -> PathBuf {
    viewports_dir(state_dir).join(format!("{slot_id}.json"))
}

pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill with signal 0 checks process existence without sending a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

pub fn list_live_ptys(state_dir: &Path) -> Vec<PtySessionMeta> {
    let dir = ptys_dir(state_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(meta) = serde_json::from_slice::<PtySessionMeta>(&bytes) else {
            continue;
        };
        let sock = pty_sock_path(state_dir, &meta.pty_id);
        if pid_alive(meta.host_pid) && pid_alive(meta.child_pid) && sock.exists() {
            out.push(meta);
        } else {
            let _ = fs::remove_file(&path);
            let _ = fs::remove_file(&sock);
        }
    }
    out.sort_by_key(|m| (m.created_at, m.pty_id.clone()));
    out
}

pub fn list_live_viewports(state_dir: &Path) -> Vec<ViewportBinding> {
    let dir = viewports_dir(state_dir);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let now = unix_now();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let Ok(vp) = serde_json::from_slice::<ViewportBinding>(&bytes) else {
            continue;
        };
        let recently_created = (now - vp.updated_at).abs() <= 6;
        if vp.pty_id != VIEWPORT_CLOSE_SENTINEL && (pid_alive(vp.attach_pid) || recently_created) {
            out.push(vp);
        } else if !recently_created {
            let _ = fs::remove_file(&path);
        }
    }
    out.sort_by(|a, b| a.slot_id.cmp(&b.slot_id));
    out
}

pub fn write_viewport_binding(state_dir: &Path, binding: &ViewportBinding) -> Result<()> {
    let dir = viewports_dir(state_dir);
    fs::create_dir_all(&dir)?;
    let path = viewport_meta_path(state_dir, &binding.slot_id);
    let bytes = serde_json::to_vec(binding)?;
    files::atomic_write(&path, &bytes)?;
    if binding.attach_pid > 0 && binding.attach_pid != std::process::id() {
        // Wake `ghostback attach` from poll() immediately so swaps/detach happen in 0 ms.
        // SAFETY: SIGWINCH default disposition is ignore; attach installs a no-op handler to interrupt poll.
        unsafe {
            libc::kill(binding.attach_pid as i32, libc::SIGWINCH);
        }
    }
    Ok(())
}

/// Reassigns an active viewport (`slot_id`) to display `new_pty_id` (or `"CLOSE"` to close the viewport split).
pub fn reassign_viewport(state_dir: &Path, slot_id: &str, new_pty_id: &str) -> Result<()> {
    let path = viewport_meta_path(state_dir, slot_id);
    let existing = fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<ViewportBinding>(&b).ok());
    let binding = ViewportBinding {
        slot_id: slot_id.to_owned(),
        pty_id: new_pty_id.to_owned(),
        attach_pid: existing.as_ref().map(|e| e.attach_pid).unwrap_or(0),
        viewport_tty: existing.map(|e| e.viewport_tty).unwrap_or_default(),
        updated_at: unix_now(),
    };
    write_viewport_binding(state_dir, &binding)
}

/// Swaps the PTY sessions displayed by two active viewports (`slot_a` and `slot_b`) in 0 ms.
pub fn swap_viewports(state_dir: &Path, slot_a: &str, slot_b: &str) -> Result<()> {
    if slot_a == slot_b {
        return Ok(());
    }
    let path_a = viewport_meta_path(state_dir, slot_a);
    let path_b = viewport_meta_path(state_dir, slot_b);
    let mut vp_a: ViewportBinding = serde_json::from_slice(&fs::read(&path_a)?)?;
    let mut vp_b: ViewportBinding = serde_json::from_slice(&fs::read(&path_b)?)?;
    std::mem::swap(&mut vp_a.pty_id, &mut vp_b.pty_id);
    let now = unix_now();
    vp_a.updated_at = now;
    vp_b.updated_at = now;
    write_viewport_binding(state_dir, &vp_a)?;
    write_viewport_binding(state_dir, &vp_b)?;
    Ok(())
}

/// Closes all currently active viewports while keeping their `pty-host` sessions alive in the background.
pub fn close_all_viewports(state_dir: &Path) -> Result<usize> {
    let mut count = 0;
    for vp in list_live_viewports(state_dir) {
        let _ = reassign_viewport(state_dir, &vp.slot_id, VIEWPORT_CLOSE_SENTINEL);
        count += 1;
    }
    Ok(count)
}

/// Closes any viewport split currently displaying `pty_id` while keeping the `pty-host` session alive in the background.
pub fn detach_pty_from_viewports(state_dir: &Path, pty_id: &str) -> Result<usize> {
    let mut count = 0;
    for vp in list_live_viewports(state_dir) {
        if vp.pty_id.eq_ignore_ascii_case(pty_id) {
            let _ = reassign_viewport(state_dir, &vp.slot_id, VIEWPORT_CLOSE_SENTINEL);
            count += 1;
        }
    }
    Ok(count)
}

/// Terminates the background `pty-host` session AND closes any viewport displaying it.
pub fn kill_pty(state_dir: &Path, pty_id: &str) -> Result<()> {
    let _ = detach_pty_from_viewports(state_dir, pty_id);
    let meta_path = pty_meta_path(state_dir, pty_id);
    if let Ok(bytes) = fs::read(&meta_path)
        && let Ok(meta) = serde_json::from_slice::<PtySessionMeta>(&bytes)
        && pid_alive(meta.host_pid)
    {
        if meta.child_pid > 0 && pid_alive(meta.child_pid) {
            // SAFETY: sending SIGHUP/SIGTERM to child process group and child PID.
            unsafe {
                libc::kill(-(meta.child_pid as i32), libc::SIGHUP);
                libc::kill(meta.child_pid as i32, libc::SIGTERM);
            }
        }
        if meta.host_pid > 0 {
            // SAFETY: sending SIGTERM to pty-host daemon PID.
            unsafe {
                libc::kill(meta.host_pid as i32, libc::SIGTERM);
            }
        }
    }
    let _ = fs::remove_file(&meta_path);
    let _ = fs::remove_file(pty_sock_path(state_dir, pty_id));
    Ok(())
}

pub fn build_attach_command(exe: &Path, state_dir: &Path, pty_id: &str, slot_id: &str) -> String {
    let _ = write_viewport_binding(
        state_dir,
        &ViewportBinding {
            slot_id: slot_id.to_owned(),
            pty_id: pty_id.to_owned(),
            attach_pid: 0,
            viewport_tty: String::new(),
            updated_at: unix_now(),
        },
    );
    format!(
        "{} --state-dir {} attach --pty {} --slot {}",
        shell_quote(&exe.to_string_lossy()),
        shell_quote(&state_dir.to_string_lossy()),
        shell_quote(pty_id),
        shell_quote(slot_id),
    )
}

/// Spawns a detached `ghostback pty-host` background process for `pty_id` and waits until its socket is ready.
pub fn spawn_managed_pty(
    exe: &Path,
    state_dir: &Path,
    pty_id: &str,
    cwd: &Path,
    label: &str,
    shell_cmd: Option<&str>,
) -> Result<PtySessionMeta> {
    let meta_path = pty_meta_path(state_dir, pty_id);
    let sock_path = pty_sock_path(state_dir, pty_id);
    if let Ok(bytes) = fs::read(&meta_path)
        && let Ok(existing) = serde_json::from_slice::<PtySessionMeta>(&bytes)
        && pid_alive(existing.host_pid)
        && pid_alive(existing.child_pid)
        && sock_path.exists()
    {
        return Ok(existing);
    }

    fs::create_dir_all(ptys_dir(state_dir))?;
    let mut cmd = Command::new(exe);
    cmd.arg("--state-dir")
        .arg(state_dir)
        .arg("pty-host")
        .arg("--id")
        .arg(pty_id)
        .arg("--cwd")
        .arg(cwd)
        .arg("--label")
        .arg(label);
    if let Some(c) = shell_cmd.filter(|s| !s.trim().is_empty()) {
        cmd.arg("--").arg(c);
    }
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    cmd.spawn()?;

    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(2500) {
        if sock_path.exists()
            && let Ok(bytes) = fs::read(&meta_path)
            && let Ok(meta) = serde_json::from_slice::<PtySessionMeta>(&bytes)
        {
            return Ok(meta);
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    Err(invalid(format!(
        "timed out waiting for background PTY host {pty_id} to start"
    )))
}

fn default_user_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| s.starts_with('/') && !s.chars().any(char::is_control))
        .unwrap_or_else(|| "/bin/zsh".into())
}

struct PtyTransport {
    master_fd: libc::c_int,
    master_write_fd: libc::c_int,
    slave_in_fd: libc::c_int,
    slave_out_fd: libc::c_int,
    is_pty: bool,
    slave_tty: String,
}

fn allocate_pty_or_pipe(ws: &mut libc::winsize) -> Result<PtyTransport> {
    let mut pty_master: libc::c_int = -1;
    let mut pty_slave: libc::c_int = -1;
    // SAFETY: openpty initializes pty_master and pty_slave with the provided winsize.
    let (master_fd, master_write_fd, slave_in_fd, slave_out_fd, is_pty) = if unsafe {
        libc::openpty(
            &mut pty_master,
            &mut pty_slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            ws,
        )
    } == 0
    {
        (pty_master, pty_master, pty_slave, pty_slave, true)
    } else {
        let mut p_in = [-1; 2];
        let mut p_out = [-1; 2];
        // SAFETY: pipe initializes the two 2-element fd arrays when openpty is restricted by sandbox.
        if unsafe { libc::pipe(p_in.as_mut_ptr()) != 0 || libc::pipe(p_out.as_mut_ptr()) != 0 } {
            return Err(io::Error::last_os_error().into());
        }
        (p_out[0], p_in[1], p_in[0], p_out[1], false)
    };

    let slave_tty = if is_pty {
        let mut buf = [0i8; 128];
        // SAFETY: ttyname_r writes a NUL-terminated path into buf.
        let rc = unsafe { libc::ttyname_r(slave_in_fd, buf.as_mut_ptr(), buf.len()) };
        if rc == 0 {
            // SAFETY: buf is NUL-terminated by ttyname_r on success.
            let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
            let raw = cstr.to_string_lossy();
            raw.strip_prefix("/dev/").unwrap_or(&raw).to_owned()
        } else {
            "tty-pty".to_owned()
        }
    } else {
        "tty-pipe".to_owned()
    };

    // Set O_NONBLOCK and FD_CLOEXEC on master descriptors
    // SAFETY: master_fd and master_write_fd are valid open descriptors.
    unsafe {
        for fd in [master_fd, master_write_fd] {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
            let fd_flags = libc::fcntl(fd, libc::F_GETFD);
            if fd_flags >= 0 {
                libc::fcntl(fd, libc::F_SETFD, fd_flags | libc::FD_CLOEXEC);
            }
        }
    }

    Ok(PtyTransport {
        master_fd,
        master_write_fd,
        slave_in_fd,
        slave_out_fd,
        is_pty,
        slave_tty,
    })
}

fn spawn_pty_child(
    transport: &PtyTransport,
    state_dir: &Path,
    pty_id: &str,
    host_key: &str,
    cwd: &Path,
    cmd_args: &[String],
) -> Result<std::process::Child> {
    let joined_cmd = cmd_args.join(" ");
    let shell = default_user_shell();
    let mut child_cmd = if joined_cmd.trim().is_empty() {
        let mut c = Command::new(&shell);
        c.arg("-l").arg("-i");
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(&joined_cmd);
        c
    };

    // Duplicate slave descriptors for stdin, stdout, stderr of child
    // SAFETY: slave_in_fd and slave_out_fd are valid descriptors; dup creates independent descriptors owned by Stdio.
    let (in_fd, out_fd, err_fd) = unsafe {
        let a = libc::dup(transport.slave_in_fd);
        let b = libc::dup(transport.slave_out_fd);
        let c = libc::dup(transport.slave_out_fd);
        if a < 0 || b < 0 || c < 0 {
            return Err(io::Error::last_os_error().into());
        }
        (a, b, c)
    };
    // Close original slave fds before spawning so child only inherits in_fd/out_fd/err_fd
    // SAFETY: closing parent's slave descriptors after duplicating them.
    unsafe {
        libc::close(transport.slave_in_fd);
        if transport.slave_out_fd != transport.slave_in_fd {
            libc::close(transport.slave_out_fd);
        }
    }
    let is_pty = transport.is_pty;
    // SAFETY: in_fd, out_fd, err_fd are freshly duplicated descriptors.
    unsafe {
        use std::os::fd::FromRawFd;
        child_cmd
            .stdin(Stdio::from_raw_fd(in_fd))
            .stdout(Stdio::from_raw_fd(out_fd))
            .stderr(Stdio::from_raw_fd(err_fd));
        child_cmd.pre_exec(move || {
            libc::setsid();
            if is_pty {
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            }
            Ok(())
        });
    }
    child_cmd
        .current_dir(cwd)
        .env("GHOSTBACK_TERMINAL_KEY", pty_id)
        .env("GHOSTBACK_HOST_KEY", host_key)
        .env("GHOSTBACK_PTY_ID", pty_id)
        .env("GHOSTBACK_STATE_DIR", state_dir)
        .env("TERM", "xterm-256color")
        .env("COLORTERM", "truecolor");

    Ok(child_cmd.spawn()?)
}

fn bind_pty_listener(sock_path: &Path) -> Option<UnixListener> {
    let _ = fs::remove_file(sock_path);
    match UnixListener::bind(sock_path) {
        Ok(l) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(sock_path, fs::Permissions::from_mode(0o600));
            }
            let _ = l.set_nonblocking(true);
            Some(l)
        }
        Err(_) => {
            let _ = fs::write(sock_path, b"pipe");
            None
        }
    }
}

struct ViewportClient {
    stream: UnixStream,
    in_buf: Vec<u8>,
}

/// Runs the background PTY host daemon (`ghostback pty-host`).
pub fn run_pty_host(
    state_dir: &Path,
    pty_id: &str,
    cwd: &Path,
    label: &str,
    cmd_args: &[String],
) -> Result<()> {
    fs::create_dir_all(ptys_dir(state_dir))?;
    // Detach into a new session so closing the parent terminal never SIGHUPs pty-host.
    // SAFETY: setsid has no memory safety preconditions.
    unsafe {
        libc::setsid();
    }

    let mut ws = libc::winsize {
        ws_row: DEFAULT_PTY_ROWS,
        ws_col: DEFAULT_PTY_COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let transport = allocate_pty_or_pipe(&mut ws)?;
    let host_pid = std::process::id();
    let host_key = crate::native::process(host_pid)
        .map(|p| p.key())
        .unwrap_or_default();

    let mut child = spawn_pty_child(&transport, state_dir, pty_id, &host_key, cwd, cmd_args)?;
    let child_pid = child.id();

    let sock_path = pty_sock_path(state_dir, pty_id);
    let listener = bind_pty_listener(&sock_path);

    let meta = PtySessionMeta {
        pty_id: pty_id.to_owned(),
        host_pid,
        child_pid,
        slave_tty: transport.slave_tty,
        cwd: cwd.to_path_buf(),
        label: label.to_owned(),
        created_at: unix_now(),
    };
    let meta_path = pty_meta_path(state_dir, pty_id);
    files::atomic_write(&meta_path, &serde_json::to_vec(&meta)?)?;

    let mut clients: Vec<ViewportClient> = Vec::new();
    let mut replay_buf: Vec<u8> = Vec::with_capacity(16_384);
    let mut read_chunk = [0u8; 16_384];

    loop {
        if let Some(ref l) = listener
            && let Ok((mut stream, _)) = l.accept()
        {
            let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
            let _ = stream.set_nonblocking(false);
            if !replay_buf.is_empty() {
                let _ = stream.write_all(&replay_buf);
                let _ = stream.flush();
            }
            let _ = stream.set_nonblocking(true);
            clients.push(ViewportClient {
                stream,
                in_buf: Vec::with_capacity(512),
            });
        }

        let mut poll_fds = Vec::with_capacity(2 + clients.len());
        poll_fds.push(libc::pollfd {
            fd: transport.master_fd,
            events: libc::POLLIN,
            revents: 0,
        });
        let client_offset = if let Some(ref l) = listener {
            poll_fds.push(libc::pollfd {
                fd: l.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
            2usize
        } else {
            1usize
        };
        for cl in &clients {
            poll_fds.push(libc::pollfd {
                fd: cl.stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            });
        }

        // SAFETY: poll_fds is a valid slice of open descriptors.
        let _ = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as _, 60) };

        // 1. Read from PTY master_fd
        let mut master_eof = false;
        if (poll_fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR)) != 0 {
            loop {
                // SAFETY: reading into stack buffer from valid master_fd.
                let n = unsafe {
                    libc::read(
                        transport.master_fd,
                        read_chunk.as_mut_ptr().cast(),
                        read_chunk.len(),
                    )
                };
                if n > 0 {
                    let slice = &read_chunk[..n as usize];
                    if replay_buf.len() + slice.len() > MAX_REPLAY_BYTES {
                        let excess = (replay_buf.len() + slice.len()) - MAX_REPLAY_BYTES;
                        replay_buf.drain(..excess.min(replay_buf.len()));
                    }
                    replay_buf.extend_from_slice(slice);
                    clients.retain_mut(|cl| write_all_nonblocking(&mut cl.stream, slice));
                } else if n == 0 {
                    master_eof = true;
                    break;
                } else {
                    let err = io::Error::last_os_error();
                    if err.kind() != io::ErrorKind::WouldBlock
                        && err.kind() != io::ErrorKind::Interrupted
                    {
                        master_eof = true;
                    }
                    break;
                }
            }
        }

        // 2. Read framed input / resize commands from attached viewport clients
        let mut i = 0;
        while i < clients.len() {
            let revents = poll_fds
                .get(client_offset + i)
                .map(|p| p.revents)
                .unwrap_or(0);
            let mut drop_client = (revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0;
            if (revents & libc::POLLIN) != 0 {
                let mut cbuf = [0u8; 4096];
                match clients[i].stream.read(&mut cbuf) {
                    Ok(0) => drop_client = true,
                    Ok(n) => {
                        clients[i].in_buf.extend_from_slice(&cbuf[..n]);
                        process_client_frames(
                            transport.master_write_fd,
                            transport.is_pty,
                            child_pid,
                            &mut ws,
                            &mut clients[i].in_buf,
                        );
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => drop_client = true,
                }
            }
            if drop_client {
                clients.remove(i);
            } else {
                i += 1;
            }
        }

        if master_eof || matches!(child.try_wait(), Ok(Some(_))) {
            break;
        }
    }

    // SAFETY: closing master_fd and master_write_fd at daemon shutdown.
    unsafe {
        libc::close(transport.master_fd);
        if transport.master_write_fd != transport.master_fd {
            libc::close(transport.master_write_fd);
        }
    }
    let _ = fs::remove_file(&sock_path);
    let _ = fs::remove_file(&meta_path);
    Ok(())
}

fn write_all_nonblocking(stream: &mut UnixStream, mut data: &[u8]) -> bool {
    let mut spins = 0;
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => return false,
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                spins += 1;
                if spins > 20 {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return false,
        }
    }
    true
}

fn process_client_frames(
    master_write_fd: libc::c_int,
    is_pty: bool,
    child_pid: u32,
    ws: &mut libc::winsize,
    in_buf: &mut Vec<u8>,
) {
    while let Some(&tag) = in_buf.first() {
        match tag {
            FRAME_INPUT => {
                if in_buf.len() < 3 {
                    break;
                }
                let len = u16::from_be_bytes([in_buf[1], in_buf[2]]) as usize;
                if in_buf.len() < 3 + len {
                    break;
                }
                let payload = &in_buf[3..3 + len];
                write_master_all(master_write_fd, payload);
                in_buf.drain(..3 + len);
            }
            FRAME_RESIZE => {
                if in_buf.len() < 5 {
                    break;
                }
                let cols = u16::from_be_bytes([in_buf[1], in_buf[2]]).max(10);
                let rows = u16::from_be_bytes([in_buf[3], in_buf[4]]).max(4);
                in_buf.drain(..5);
                ws.ws_col = cols;
                ws.ws_row = rows;
                if is_pty {
                    // SAFETY: updating PTY window size and signaling foreground process group to redraw.
                    unsafe {
                        libc::ioctl(master_write_fd, libc::TIOCSWINSZ, ws as *const _);
                        let pgrp = libc::tcgetpgrp(master_write_fd);
                        if pgrp > 0 {
                            libc::kill(-pgrp, libc::SIGWINCH);
                        }
                        if child_pid > 0 && pgrp != child_pid as i32 {
                            libc::kill(-(child_pid as i32), libc::SIGWINCH);
                        }
                    }
                }
            }
            _ => {
                // Unknown frame byte; discard to resynchronize.
                in_buf.drain(..1);
            }
        }
    }
}

fn write_master_all(master_fd: libc::c_int, mut buf: &[u8]) {
    let mut spins = 0;
    while !buf.is_empty() {
        // SAFETY: writing slice bytes to valid master_fd.
        let n = unsafe { libc::write(master_fd, buf.as_ptr().cast(), buf.len()) };
        if n > 0 {
            buf = &buf[n as usize..];
        } else {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            if err.kind() == io::ErrorKind::WouldBlock && spins < 20 {
                spins += 1;
                std::thread::sleep(Duration::from_millis(1));
                continue;
            }
            break;
        }
    }
}

struct RawTermGuard {
    original: Option<libc::termios>,
}

impl RawTermGuard {
    fn enter() -> Self {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::zeroed();
        // SAFETY: reading termios if STDIN_FILENO is a TTY.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) } != 0 {
            return Self { original: None };
        }
        // SAFETY: tcgetattr succeeded.
        let original = unsafe { termios.assume_init() };
        let mut raw = original;
        // SAFETY: cfmakeraw configures raw pass-through mode so Ctrl+C / escape sequences reach the PTY.
        unsafe {
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw);
        }
        Self {
            original: Some(original),
        }
    }
}

impl Drop for RawTermGuard {
    fn drop(&mut self) {
        let _ = io::stdout().write_all(TERM_RESET_MODES);
        let _ = io::stdout().flush();
        if let Some(ref orig) = self.original {
            // SAFETY: restoring saved termios on exit.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, orig);
            }
        }
    }
}

extern "C" fn noop_sigwinch(_: libc::c_int) {}

fn current_winsize() -> (u16, u16) {
    let mut ws = libc::winsize {
        ws_row: 0,
        ws_col: 0,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: querying STDOUT_FILENO terminal dimensions.
    if unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0
        && ws.ws_col > 0
        && ws.ws_row > 0
    {
        (ws.ws_col, ws.ws_row)
    } else {
        (100, 32)
    }
}

fn send_resize_frame(stream: &mut UnixStream, cols: u16, rows: u16) -> io::Result<()> {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    stream.write_all(&[FRAME_RESIZE, c[0], c[1], r[0], r[1]])?;
    stream.flush()
}

fn send_input_frame(stream: &mut UnixStream, data: &[u8]) -> io::Result<()> {
    for chunk in data.chunks(16_384) {
        let len = (chunk.len() as u16).to_be_bytes();
        stream.write_all(&[FRAME_INPUT, len[0], len[1]])?;
        stream.write_all(chunk)?;
    }
    stream.flush()
}

fn connect_viewport_to_pty(state_dir: &Path, pty_id: &str) -> Result<(UnixStream, (u16, u16))> {
    let sock_path = pty_sock_path(state_dir, pty_id);
    let started = Instant::now();
    let mut stream = loop {
        match UnixStream::connect(&sock_path) {
            Ok(s) => break s,
            Err(_) if started.elapsed() < Duration::from_millis(1500) => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(e.into()),
        }
    };
    let size = current_winsize();
    let _ = send_resize_frame(&mut stream, size.0, size.1);
    stream.set_nonblocking(true)?;
    Ok((stream, size))
}

/// Runs the viewport client inside a Ghostty split (`ghostback attach --pty <id> --slot <slot>`).
pub fn run_attach(state_dir: &Path, initial_pty_id: &str, slot_opt: Option<&str>) -> Result<()> {
    // SAFETY: installing a no-op SIGWINCH handler so window resize and viewport swap/close wake poll() in 0 ms.
    unsafe {
        libc::signal(
            libc::SIGWINCH,
            noop_sigwinch as *const () as libc::sighandler_t,
        );
    }
    let _guard = RawTermGuard::enter();
    let slot_id = slot_opt
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("vp-{}", std::process::id()));
    let viewport_tty = crate::macos::current_tty().unwrap_or_default();
    let mut current_pty_id = initial_pty_id.to_owned();

    let binding = ViewportBinding {
        slot_id: slot_id.clone(),
        pty_id: current_pty_id.clone(),
        attach_pid: std::process::id(),
        viewport_tty: viewport_tty.clone(),
        updated_at: unix_now(),
    };
    let _ = write_viewport_binding(state_dir, &binding);

    let (mut stream, mut last_size) = connect_viewport_to_pty(state_dir, &current_pty_id)?;
    let mut last_check = Instant::now();
    let mut stdin_buf = [0u8; 4096];
    let mut sock_buf = [0u8; 16_384];

    loop {
        let mut pfds = [
            libc::pollfd {
                fd: libc::STDIN_FILENO,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: stream.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: polling STDIN_FILENO and stream descriptor.
        let poll_rc = unsafe { libc::poll(pfds.as_mut_ptr(), 2, 65) };

        if (pfds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0 {
            break;
        }
        if (pfds[0].revents & libc::POLLIN) != 0 {
            // SAFETY: reading raw stdin bytes from STDIN_FILENO.
            let n = unsafe {
                libc::read(
                    libc::STDIN_FILENO,
                    stdin_buf.as_mut_ptr().cast(),
                    stdin_buf.len(),
                )
            };
            if n <= 0 {
                break;
            }
            if send_input_frame(&mut stream, &stdin_buf[..n as usize]).is_err() {
                break;
            }
        }

        let mut pty_closed =
            (pfds[1].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL)) != 0;
        if (pfds[1].revents & libc::POLLIN) != 0 {
            loop {
                match stream.read(&mut sock_buf) {
                    Ok(0) => {
                        pty_closed = true;
                        break;
                    }
                    Ok(n) => {
                        let mut out = io::stdout().lock();
                        let _ = out.write_all(&sock_buf[..n]);
                        let _ = out.flush();
                    }
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::Interrupted =>
                    {
                        break;
                    }
                    Err(_) => {
                        pty_closed = true;
                        break;
                    }
                }
            }
        }

        if poll_rc < 0 || last_check.elapsed() >= Duration::from_millis(55) {
            last_check = Instant::now();
            let now_size = current_winsize();
            if now_size != last_size {
                last_size = now_size;
                let _ = send_resize_frame(&mut stream, now_size.0, now_size.1);
            }

            let vp_path = viewport_meta_path(state_dir, &slot_id);
            if let Ok(bytes) = fs::read(&vp_path)
                && let Ok(vp) = serde_json::from_slice::<ViewportBinding>(&bytes)
                && vp.pty_id != current_pty_id
            {
                if vp.pty_id == VIEWPORT_CLOSE_SENTINEL {
                    break;
                }
                if let Ok((new_stream, new_size)) = connect_viewport_to_pty(state_dir, &vp.pty_id) {
                    current_pty_id = vp.pty_id.clone();
                    stream = new_stream;
                    last_size = new_size;
                    let mut out = io::stdout().lock();
                    let _ = out.write_all(TERM_CLEAR_VIEWPORT);
                    let _ = out.flush();
                    let _ = write_viewport_binding(
                        state_dir,
                        &ViewportBinding {
                            slot_id: slot_id.clone(),
                            pty_id: current_pty_id.clone(),
                            attach_pid: std::process::id(),
                            viewport_tty: viewport_tty.clone(),
                            updated_at: unix_now(),
                        },
                    );
                    pty_closed = false;
                }
            }
        }

        if pty_closed {
            break;
        }
    }

    let _ = fs::remove_file(viewport_meta_path(state_dir, &slot_id));
    Ok(())
}
