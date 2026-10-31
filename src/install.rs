//! Transactional installation. Configuration edits are owned and reversible.
use crate::distribution::{self, Distribution};
use crate::sessions::shell_quote;
use crate::{Result, app, files, invalid, process};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BEGIN: &str = "# >>> ghostback >>>";
const END: &str = "# <<< ghostback <<<";
const LABEL: &str = "dev.ghostback.agent";
const MAX_BINARY: u64 = 10 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Kind {
    Block,
    Hook { agent: String, command: String },
    Owned { digest: String },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InstalledFile {
    pub path: PathBuf,
    pub kind: Kind,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub generation: String,
    pub home: PathBuf,
    pub binary: PathBuf,
    #[serde(default)]
    pub distribution: Distribution,
    pub codex_binary: Option<PathBuf>,
    pub files: Vec<InstalledFile>,
}
#[derive(Serialize, Deserialize)]
struct JournalEntry {
    path: PathBuf,
    backup: Option<PathBuf>,
    after_digest: Option<String>,
    #[serde(default)]
    mode: Option<u32>,
}
#[derive(Serialize, Deserialize)]
struct Journal {
    manifest_digest: String,
    home: PathBuf,
    entries: Vec<JournalEntry>,
}
struct Edit {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    kind: Kind,
    executable: bool,
    mode: Option<u32>,
}
pub struct Options {
    pub home: PathBuf,
    pub state: PathBuf,
    pub source: PathBuf,
    pub dry_run: bool,
    pub launch: bool,
}

pub fn digest(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    Sha256::digest(bytes)
        .iter()
        .flat_map(|byte| {
            [
                char::from(HEX[(byte >> 4) as usize]),
                char::from(HEX[(byte & 0x0f) as usize]),
            ]
        })
        .collect()
}
fn optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match files::read(path, MAX_BINARY) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e)
            if e.downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            Ok(None)
        }
        Err(e) => Err(e),
    }
}
fn text(path: &Path) -> Result<String> {
    Ok(String::from_utf8(optional(path)?.unwrap_or_default())?)
}

pub fn marked(content: &str, body: Option<&str>) -> Result<String> {
    let starts = content.matches(BEGIN).count();
    let ends = content.matches(END).count();
    if starts != ends || starts > 1 {
        return Err(invalid("configuration has malformed Ghostback markers"));
    }
    let mut output = if let (Some(start), Some(end)) = (content.find(BEGIN), content.find(END)) {
        if end < start {
            return Err(invalid("configuration markers are reversed"));
        }
        if start > 0 && content.as_bytes()[start - 1] != b'\n' {
            return Err(invalid("Ghostback marker is not on its own line"));
        }
        let mut finish = end + END.len();
        if content.as_bytes().get(finish) == Some(&b'\n') {
            finish += 1;
        }
        format!("{}{}", &content[..start], &content[finish..])
    } else {
        content.to_owned()
    };
    if let Some(body) = body {
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&format!("{BEGIN}\n{body}\n{END}\n"));
    }
    Ok(output)
}

pub fn merge_hook(mut value: Value, agent: &str, command: &str, remove: bool) -> Result<Value> {
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("agent configuration must be an object"))?;
    if agent == "agy" {
        let owned = object.get("ghostback").is_some_and(|v| {
            v.get("description").and_then(Value::as_str) == Some("Managed by Ghostback")
                && v.get("SessionStart")
                    .and_then(Value::as_array)
                    .is_some_and(|rows| {
                        rows.iter()
                            .any(|r| r.get("command").and_then(Value::as_str) == Some(command))
                    })
        });
        if remove {
            if owned {
                object.remove("ghostback");
            }
            return Ok(value);
        }
        if object.contains_key("ghostback") && !owned {
            return Err(invalid("an unrelated hook is already named ghostback"));
        }
        object.insert("ghostback".into(),json!({"description":"Managed by Ghostback","SessionStart":[{"type":"command","command":command,"timeout":10}],"PreInvocation":[{"type":"command","command":command,"timeout":10}]}));
        return Ok(value);
    }
    if remove && !object.contains_key("hooks") {
        return Ok(value);
    }
    let hooks = object
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| invalid("hooks must be an object"))?;
    for event in ["SessionStart", "UserPromptSubmit"] {
        if remove && !hooks.contains_key(event) {
            continue;
        }
        let groups = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| invalid("hook event must be an array"))?;
        groups.retain_mut(|group| {
            if let Some(entries) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                let before = entries.len();
                entries
                    .retain(|entry| entry.get("command").and_then(Value::as_str) != Some(command));
                if before > entries.len() && entries.is_empty() {
                    return false;
                }
            }
            true
        });
        if !remove {
            groups.push(json!({"hooks":[{"type":"command","command":command,"timeout":10}]}));
        }
        if groups.is_empty() {
            hooks.remove(event);
        }
    }
    if hooks.is_empty() {
        object.remove("hooks");
    }
    Ok(value)
}

pub fn shell_body(binary: &Path, state: &Path, bash: bool) -> String {
    let bin = shell_quote(&binary.to_string_lossy());
    let root = shell_quote(&state.to_string_lossy());
    let local = shell_quote(&binary.parent().unwrap_or(Path::new("/")).to_string_lossy());
    let shims = shell_quote(&state.join("shims").to_string_lossy());
    let prompt = format!("command {bin} --state-dir {root} shell-ready --shell-pid \"$$\" || true");
    if bash {
        format!(
            r#"if [ "${{_GHOSTBACK_LOADED:-}}" != "$$" ]; then
  _GHOSTBACK_LOADED=$$
  _ghostback_prompt() {{
    local _gb_status=$?
    if [ -n "${{GHOSTTY_SURFACE_ID:-}}" ]; then {prompt}; fi
    return "$_gb_status"
  }}
  if declare -p PROMPT_COMMAND 2>/dev/null | grep -q 'declare -a'; then
    PROMPT_COMMAND+=(_ghostback_prompt)
  else
    if [ -n "${{PROMPT_COMMAND:-}}" ]; then
      PROMPT_COMMAND="$PROMPT_COMMAND"$'\n''_ghostback_prompt'
    else
      PROMPT_COMMAND=_ghostback_prompt
    fi
  fi
  if [ -n "${{GHOSTTY_SURFACE_ID:-}}" ]; then export PATH={shims}:{local}:"$PATH"; fi
fi"#
        )
    } else {
        format!(
            r#"if [[ ${{_GHOSTBACK_LOADED:-}} != $$ ]]; then
  typeset -g _GHOSTBACK_LOADED=$$
  _ghostback_prompt() {{
    local _gb_status=$?
    if [[ -n ${{GHOSTTY_SURFACE_ID:-}} ]]; then {prompt}; fi
    return $_gb_status
  }}
  autoload -Uz add-zsh-hook
  add-zsh-hook precmd _ghostback_prompt
  if [[ -n ${{GHOSTTY_SURFACE_ID:-}} ]]; then export PATH={shims}:{local}:"$PATH"; fi
fi"#
        )
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
pub fn launch_agent(binary: &Path, state: &Path, home: &Path, path: &str) -> String {
    let roots = installation_roots(home);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>Label</key><string>{LABEL}</string>
<key>ProgramArguments</key><array><string>{}</string><string>--state-dir</string><string>{}</string><string>watch</string></array>
<key>RunAtLoad</key><true/><key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict><key>ThrottleInterval</key><integer>10</integer>
<key>EnvironmentVariables</key><dict><key>HOME</key><string>{}</string><key>PATH</key><string>{}</string>
<key>CODEX_HOME</key><string>{}</string><key>CLAUDE_CONFIG_DIR</key><string>{}</string><key>GROK_HOME</key><string>{}</string></dict>
<key>StandardOutPath</key><string>/dev/null</string><key>StandardErrorPath</key><string>/dev/null</string></dict></plist>
"#,
        xml(&binary.to_string_lossy()),
        xml(&state.to_string_lossy()),
        xml(&home.to_string_lossy()),
        xml(path),
        xml(&roots.codex.to_string_lossy()),
        xml(&roots.claude.to_string_lossy()),
        xml(&roots.grok.to_string_lossy())
    )
}

fn installation_roots(home: &Path) -> crate::capture::Roots {
    if app::home().is_ok_and(|actual| actual == home) {
        crate::capture::Roots::from_home(home)
    } else {
        crate::capture::Roots {
            claude: home.join(".claude"),
            codex: home.join(".codex"),
            grok: home.join(".grok"),
        }
    }
}

fn make_edit(path: PathBuf, after: Vec<u8>, kind: Kind, executable: bool) -> Result<Edit> {
    validate_path(&path)?;
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        path.metadata().ok().map(|m| m.permissions().mode())
    };
    #[cfg(not(unix))]
    let mode = None;
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(invalid(format!(
            "refusing to edit symlink: {}",
            path.display()
        )));
    }
    Ok(Edit {
        before: optional(&path)?,
        path,
        after: Some(after),
        kind,
        executable,
        mode,
    })
}
fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|s| s.chars().any(char::is_control))
    {
        return Err(invalid(
            "installation paths must be absolute UTF-8 paths without control characters",
        ));
    }
    Ok(())
}

pub fn setup(options: &Options) -> Result<Vec<PathBuf>> {
    if options.launch && !cfg!(target_os = "macos") {
        return Err(invalid("native installation supports macOS"));
    }
    for path in [&options.home, &options.state, &options.source] {
        validate_path(path)?;
    }
    let home = &options.home;
    let state = &options.state;
    let location = distribution::installation_binary(&options.source, home)?;
    let binary = location.path;
    let manifest_path = state.join("install.json");
    let _lock = if options.dry_run {
        None
    } else {
        Some(files::Lock::acquire(&state.join("install.lock"))?)
    };
    if !options.dry_run && state.join("setup-pending.json").is_file() {
        recover_unlocked(state, home)?;
    }
    let previous = optional(&manifest_path)?
        .map(|bytes| serde_json::from_slice::<Manifest>(&bytes))
        .transpose()?;
    if previous.as_ref().is_some_and(|m| m.home != *home) {
        return Err(invalid("installation manifest belongs to another home"));
    }
    let mut edits = Vec::new();
    let mut own = |path: PathBuf, after: Vec<u8>, executable: bool| -> Result<()> {
        if let Some(current) = optional(&path)? {
            let owned = previous.as_ref().is_some_and(|m| {
                m.files.iter().any(|f| {
                    f.path == path
                        && matches!(&f.kind,Kind::Owned{digest:hash}if hash==&digest(&current))
                })
            });
            if !owned && path != options.source {
                return Err(invalid(format!(
                    "refusing to overwrite an unowned or changed file: {}",
                    path.display()
                )));
            }
        }
        let hash = digest(&after);
        edits.push(make_edit(
            path,
            after,
            Kind::Owned { digest: hash },
            executable,
        )?);
        Ok(())
    };
    if location.distribution == Distribution::Standalone {
        own(
            binary.clone(),
            files::read(&options.source, MAX_BINARY)?,
            true,
        )?;
    }
    let shim = state.join("shims/codex");
    let codex = app::executable("codex")
        .filter(|p| p != &shim)
        .or_else(|| previous.as_ref().and_then(|m| m.codex_binary.clone()));
    if let Some(codex) = &codex {
        validate_path(codex)?;
        let body = format!(
            "#!/bin/sh\nexec {} --state-dir {} run --binary {} -- \"$@\"\n",
            shell_quote(&binary.to_string_lossy()),
            shell_quote(&state.to_string_lossy()),
            shell_quote(&codex.to_string_lossy())
        );
        own(shim.clone(), body.into_bytes(), true)?;
    }
    for (path, bash) in [
        (home.join(".zshrc"), false),
        (home.join(".bashrc"), true),
        (home.join(".bash_profile"), true),
    ] {
        let after = marked(&text(&path)?, Some(&shell_body(&binary, state, bash)))?;
        edits.push(make_edit(path, after.into_bytes(), Kind::Block, false)?);
    }
    if let Some(root) = std::env::var_os("ZDOTDIR")
        .filter(|_| app::home().is_ok_and(|actual| actual == *home))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
    {
        if !root.starts_with(home) {
            return Err(invalid("ZDOTDIR must be inside the selected home"));
        }
        let path = root.join(".zshrc");
        if path != home.join(".zshrc") {
            edits.push(make_edit(
                path.clone(),
                marked(&text(&path)?, Some(&shell_body(&binary, state, false)))?.into_bytes(),
                Kind::Block,
                false,
            )?);
        }
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|_| app::home().is_ok_and(|actual| actual == *home))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let config = home.join("Library/Application Support/com.mitchellh.ghostty/config");
    let config = if config.is_file() {
        config
    } else {
        xdg.join("ghostty/config")
    };
    edits.push(make_edit(
        config.clone(),
        marked(&text(&config)?, Some("window-save-state = always"))?.into_bytes(),
        Kind::Block,
        false,
    )?);
    let roots = installation_roots(home);
    for root in [&roots.claude, &roots.codex, &roots.grok] {
        validate_path(root)?;
    }
    for (agent, path) in [
        ("claude", roots.claude.join("settings.json")),
        ("codex", roots.codex.join("hooks.json")),
        ("agy", home.join(".gemini/config/hooks.json")),
    ] {
        let input = text(&path)?;
        let mut value = if input.trim().is_empty() {
            json!({})
        } else {
            serde_json::from_str(&input)?
        };
        if let Some(previous) = &previous {
            for file in &previous.files {
                if file.path == path
                    && let Kind::Hook {
                        agent: old_agent,
                        command,
                    } = &file.kind
                {
                    value = merge_hook(value, old_agent, command, true)?;
                }
            }
        }
        let command = format!(
            "{} --state-dir {} hook {agent}",
            shell_quote(&binary.to_string_lossy()),
            shell_quote(&state.to_string_lossy())
        );
        let after = serde_json::to_vec_pretty(&merge_hook(value, agent, &command, false)?)?;
        edits.push(make_edit(
            path,
            after,
            Kind::Hook {
                agent: agent.into(),
                command,
            },
            false,
        )?);
    }
    let path = std::env::var_os("PATH")
        .map(|p| {
            std::env::split_paths(&p)
                .filter(|p| p.is_absolute() && p != &state.join("shims"))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let path = std::env::join_paths(path)?.to_string_lossy().into_owned();
    let plist = home.join("Library/LaunchAgents/dev.ghostback.agent.plist");
    let plist_bytes = launch_agent(&binary, state, home, &path).into_bytes();
    let plist_hash = digest(&plist_bytes);
    if let Some(current) = optional(&plist)?
        && !previous.as_ref().is_some_and(|m| {
            m.files.iter().any(|f| {
                f.path == plist
                    && matches!(&f.kind,Kind::Owned{digest:hash}if hash==&digest(&current))
            })
        })
    {
        return Err(invalid(
            "LaunchAgent file was changed or belongs to another installation",
        ));
    }
    edits.push(make_edit(
        plist.clone(),
        plist_bytes,
        Kind::Owned { digest: plist_hash },
        false,
    )?);
    let manifest = Manifest {
        generation: format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ),
        home: home.clone(),
        binary: binary.clone(),
        distribution: location.distribution,
        codex_binary: codex,
        files: edits
            .iter()
            .map(|e| InstalledFile {
                path: e.path.clone(),
                kind: e.kind.clone(),
            })
            .collect(),
    };
    if let Some(previous) = &previous {
        append_retired_integrations(&mut edits, previous)?;
    }
    let paths = edits.iter().map(|e| e.path.clone()).collect::<Vec<_>>();
    if options.dry_run {
        return Ok(paths);
    }
    apply_transaction(state, home, &edits, &manifest)?;
    if options.launch {
        let domain = format!("gui/{}", current_uid());
        let _ = process::output(
            "/bin/launchctl",
            &["bootout", &format!("{domain}/{LABEL}")],
            Duration::from_secs(10),
        );
        process::output(
            "/bin/launchctl",
            &["bootstrap", &domain, &plist.to_string_lossy()],
            Duration::from_secs(15),
        )?;
    }
    Ok(paths)
}

fn append_retired_integrations(edits: &mut Vec<Edit>, previous: &Manifest) -> Result<()> {
    for file in &previous.files {
        if edits.iter().any(|e| e.path == file.path) {
            continue;
        }
        let Some(current) = optional(&file.path)? else {
            continue;
        };
        let after = match &file.kind {
            Kind::Owned { digest: expected } => {
                if digest(&current) != *expected {
                    continue;
                }
                None
            }
            Kind::Block => Some(marked(&String::from_utf8(current.clone())?, None)?.into_bytes()),
            Kind::Hook { agent, command } => Some(serde_json::to_vec_pretty(&merge_hook(
                serde_json::from_slice(&current)?,
                agent,
                command,
                true,
            )?)?),
        };
        let mut edit = make_edit(file.path.clone(), Vec::new(), file.kind.clone(), false)?;
        edit.before = Some(current);
        edit.after = after;
        edits.push(edit);
    }
    Ok(())
}

/// The watcher may replace itself after Homebrew changes the stable opt link.
pub fn homebrew_upgrade(state: &Path, current: &Path) -> Result<Option<PathBuf>> {
    let Some(bytes) = optional(&state.join("install.json"))? else {
        return Ok(None);
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.distribution != Distribution::Homebrew {
        return Ok(None);
    }
    let next = manifest.binary.canonicalize()?;
    let location = distribution::installation_binary(&next, &manifest.home)?;
    if location.distribution != Distribution::Homebrew || location.path != manifest.binary {
        return Err(invalid(
            "saved Homebrew path no longer identifies a managed executable",
        ));
    }
    let current = current
        .canonicalize()
        .unwrap_or_else(|_| current.to_owned());
    Ok((next != current).then_some(location.path))
}

fn apply_transaction(state: &Path, home: &Path, edits: &[Edit], manifest: &Manifest) -> Result<()> {
    for edit in edits {
        if optional(&edit.path)? != edit.before {
            return Err(invalid("configuration changed during setup; rerun setup"));
        }
    }
    let generation = format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
    );
    let backup = state.join("setup-backup").join(generation);
    fs::create_dir_all(&backup)?;
    let mut journal = Journal {
        manifest_digest: digest(&serde_json::to_vec_pretty(manifest)?),
        home: home.into(),
        entries: Vec::new(),
    };
    for (index, edit) in edits.iter().enumerate() {
        let original = if let Some(bytes) = &edit.before {
            let path = backup.join(index.to_string());
            files::atomic_write(&path, bytes)?;
            Some(path)
        } else {
            None
        };
        journal.entries.push(JournalEntry {
            path: edit.path.clone(),
            backup: original,
            after_digest: edit.after.as_deref().map(digest),
            mode: edit.mode,
        });
    }
    files::atomic_write(
        &state.join("setup-pending.json"),
        &serde_json::to_vec_pretty(&journal)?,
    )?;
    let apply = (|| -> Result<()> {
        for edit in edits {
            if optional(&edit.path)? != edit.before {
                return Err(invalid("configuration changed while setup was applying"));
            }
            if edit.before != edit.after {
                if let Some(after) = &edit.after {
                    files::atomic_write(&edit.path, after)?;
                } else {
                    fs::remove_file(&edit.path)?;
                }
            }
            #[cfg(unix)]
            if edit.executable && edit.after.is_some() {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&edit.path, fs::Permissions::from_mode(0o755))?;
            }
        }
        files::atomic_write(
            &state.join("install.json"),
            &serde_json::to_vec_pretty(manifest)?,
        )?;
        Ok(())
    })();
    if let Err(error) = apply {
        let recovery = recover_unlocked(state, home);
        if let Err(recovery) = recovery {
            return Err(invalid(format!(
                "setup failed: {error}; rollback needs attention: {recovery}"
            )));
        }
        return Err(error);
    }
    fs::remove_file(state.join("setup-pending.json"))?;
    Ok(())
}

pub fn recover(state: &Path, home: &Path) -> Result<()> {
    let _lock = files::Lock::acquire(&state.join("install.lock"))?;
    recover_unlocked(state, home)
}

fn recover_unlocked(state: &Path, home: &Path) -> Result<()> {
    let pending = state.join("setup-pending.json");
    let journal: Journal = serde_json::from_slice(&files::read(&pending, files::MAX_JSON)?)?;
    if journal.home != home {
        return Err(invalid("setup journal belongs to another home"));
    }
    if optional(&state.join("install.json"))?
        .is_some_and(|bytes| digest(&bytes) == journal.manifest_digest)
    {
        fs::remove_file(pending)?;
        return Ok(());
    }
    for entry in &journal.entries {
        validate_path(&entry.path)?;
        if let Some(backup) = &entry.backup {
            validate_path(backup)?;
            if !backup.starts_with(state.join("setup-backup"))
                || backup
                    .components()
                    .any(|part| matches!(part, std::path::Component::ParentDir))
            {
                return Err(invalid("invalid backup path"));
            }
        }
    }
    for entry in journal.entries.iter().rev() {
        if optional(&entry.path)?.as_deref().map(digest) != entry.after_digest {
            continue;
        }
        if let Some(backup) = &entry.backup {
            if !backup.starts_with(state.join("setup-backup")) {
                return Err(invalid("invalid backup path"));
            }
            files::atomic_write(&entry.path, &files::read(backup, MAX_BINARY)?)?;
            #[cfg(unix)]
            if let Some(mode) = entry.mode {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&entry.path, fs::Permissions::from_mode(mode))?;
            }
        } else {
            fs::remove_file(&entry.path)?;
        }
    }
    fs::remove_file(pending)?;
    Ok(())
}

#[cfg(unix)]
fn current_uid() -> u32 {
    // SAFETY: geteuid has no arguments or side effects.
    unsafe { libc::geteuid() }
}
#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

pub fn uninstall(state: &Path, launch: bool) -> Result<Vec<PathBuf>> {
    let _lock = files::Lock::acquire(&state.join("install.lock"))?;
    let path = state.join("install.json");
    let Some(bytes) = optional(&path)? else {
        return Ok(Vec::new());
    };
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    let mut edits = Vec::<(PathBuf, Option<Vec<u8>>)>::new();
    for file in &manifest.files {
        let Some(current) = optional(&file.path)? else {
            continue;
        };
        match &file.kind {
            Kind::Owned { digest: expected } => {
                if digest(&current) == *expected {
                    edits.push((file.path.clone(), None));
                }
            }
            Kind::Block => {
                let input = String::from_utf8(current)?;
                let output = marked(&input, None)?;
                if input != output {
                    edits.push((file.path.clone(), Some(output.into_bytes())));
                }
            }
            Kind::Hook { agent, command } => {
                let value: Value = serde_json::from_slice(&current)?;
                let after = merge_hook(value, agent, command, true)?;
                edits.push((file.path.clone(), Some(serde_json::to_vec_pretty(&after)?)));
            }
        }
    }
    if launch && cfg!(target_os = "macos") {
        let _ = process::output(
            "/bin/launchctl",
            &["bootout", &format!("gui/{}/{LABEL}", current_uid())],
            Duration::from_secs(10),
        );
    }
    let mut changed = Vec::new();
    for (path, bytes) in edits {
        if let Some(bytes) = bytes {
            files::atomic_write(&path, &bytes)?;
        } else {
            fs::remove_file(&path)?;
        }
        changed.push(path);
    }
    fs::remove_file(state.join("install.json"))?;
    Ok(changed)
}
