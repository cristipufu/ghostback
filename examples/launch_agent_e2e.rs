//! Opt-in read-only capture check under a uniquely named, temporary launchd job.
use ghostback::sessions::Store;
use ghostback::{Result, files, install, invalid, process};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Job(String);
impl Drop for Job {
    fn drop(&mut self) {
        let _ = process::output(
            "/bin/launchctl",
            &["bootout", &self.0],
            Duration::from_secs(5),
        );
    }
}
fn run() -> Result<()> {
    if !cfg!(target_os = "macos") {
        return Err(invalid("launchd fixture requires macOS"));
    }
    let binary = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("usage: launch_agent_e2e <ghostback executable>"))?
        .canonicalize()?;
    let root = tempfile::Builder::new()
        .prefix("ghostback-launchd-e2e-")
        .tempdir()?;
    let home = root.path().join("home");
    let state = root.path().join("state");
    std::fs::create_dir(&home)?;
    let label = format!(
        "dev.ghostback.test.{}",
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let path = std::env::var("PATH")?;
    let plist =
        install::launch_agent(&binary, &state, &home, &path).replace("dev.ghostback.agent", &label);
    let plist_path = root.path().join("helper.plist");
    files::atomic_write(&plist_path, plist.as_bytes())?;
    // SAFETY: geteuid has no arguments or side effects.
    #[cfg(unix)]
    let uid = unsafe { libc::geteuid() };
    #[cfg(not(unix))]
    let uid = 0;
    let domain = format!("gui/{uid}");
    process::output(
        "/bin/launchctl",
        &["bootstrap", &domain, &plist_path.to_string_lossy()],
        Duration::from_secs(10),
    )?;
    let _job = Job(format!("{domain}/{label}"));
    let store = Store::new(state)?;
    let started = Instant::now();
    loop {
        let health = store.health_status().unwrap_or_default();
        if health.get("watcher").is_some_and(|s| s == "running") {
            println!(
                "PASS: temporary launchd helper started and completed native capture without controlling terminals."
            );
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(25) {
            return Err(invalid(format!(
                "login helper did not complete capture: {health:?}"
            )));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
fn main() {
    if let Err(error) = run() {
        eprintln!("launch-agent-e2e: {error}");
        std::process::exit(1);
    }
}
