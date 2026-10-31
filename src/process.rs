//! Bounded subprocess execution without background reader threads.
use crate::{Result, invalid};
use std::io::{self, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAX_OUTPUT: usize = 1_048_576;
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[cfg(unix)]
fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<()> {
    let descriptor = pipe.as_raw_fd();
    // SAFETY: both fcntl calls operate on the live pipe descriptor we own.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn drain(pipe: &mut impl Read, bytes: &mut Vec<u8>) -> Result<bool> {
    let mut buffer = [0u8; 8192];
    for _ in 0..16 {
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(true),
            Ok(count) => {
                if bytes.len() + count > MAX_OUTPUT {
                    return Err(invalid("subprocess output exceeded the 1 MiB limit"));
                }
                bytes.extend_from_slice(&buffer[..count]);
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

#[cfg(unix)]
pub fn output(program: &str, args: &[&str], timeout: Duration) -> Result<String> {
    let started = Instant::now();
    let mut child = OwnedChild(
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let mut stdout = child
        .0
        .stdout
        .take()
        .ok_or_else(|| invalid("missing subprocess stdout"))?;
    let mut stderr = child
        .0
        .stderr
        .take()
        .ok_or_else(|| invalid("missing subprocess stderr"))?;
    nonblocking(&stdout)?;
    nonblocking(&stderr)?;
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let (mut out_done, mut err_done) = (false, false);
    let status = loop {
        if !out_done {
            out_done = drain(&mut stdout, &mut out)?;
        }
        if !err_done {
            err_done = drain(&mut stderr, &mut err)?;
        }
        if let Some(status) = child.0.try_wait()?
            && out_done
            && err_done
        {
            break status;
        }
        if started.elapsed() >= timeout {
            return Err(invalid(format!(
                "{program} timed out after {} ms",
                timeout.as_millis()
            )));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    if !status.success() {
        return Err(invalid(format!(
            "{program} failed ({status}): {}",
            String::from_utf8_lossy(&err).trim()
        )));
    }
    Ok(String::from_utf8(out)?)
}

#[cfg(not(unix))]
pub fn output(_program: &str, _args: &[&str], _timeout: Duration) -> Result<String> {
    Err(invalid(
        "native process execution currently supports Unix platforms",
    ))
}
