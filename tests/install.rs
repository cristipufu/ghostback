#![allow(clippy::unwrap_used, clippy::expect_used)]
use ghostback::files;
use ghostback::install::{self, Options, marked, merge_hook};
use serde_json::json;
use std::fs;
use std::process::Command;
use tempfile::TempDir;
fn fixture() -> (TempDir, Options) {
    let root = TempDir::new().unwrap();
    let home = root.path().join("home");
    fs::create_dir(&home).unwrap();
    let source = root.path().join("source");
    fs::write(&source, b"#!/bin/sh\nexit 0\n").unwrap();
    let options = Options {
        home,
        state: root.path().join("state"),
        source,
        dry_run: false,
        launch: false,
    };
    (root, options)
}

#[test]
fn ownership_digest_preserves_standard_sha256_hex_fingerprints() {
    assert_eq!(
        install::digest(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        install::digest(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
#[test]
fn marked_blocks_are_idempotent_and_preserve_surroundings() {
    let input = "export TEST=before\n";
    let first = marked(input, Some("managed")).unwrap();
    assert_eq!(marked(&first, Some("managed")).unwrap(), first);
    assert_eq!(marked(&first, None).unwrap(), input);
}
#[test]
fn malformed_markers_are_rejected() {
    for text in [
        "# >>> ghostback >>>\n",
        "# <<< ghostback <<<\n",
        "# <<< ghostback <<<\n# >>> ghostback >>>\n",
    ] {
        assert!(marked(text, None).is_err());
    }
}
#[test]
fn json_hooks_merge_without_losing_user_hooks() {
    let original = json!({"model":"keep","hooks":{"SessionStart":[{"hooks":[{"type":"command","command":"user command"}]}, {"hooks":[]}]}});
    let first = merge_hook(original.clone(), "claude", "managed", false).unwrap();
    assert_eq!(
        merge_hook(first.clone(), "claude", "managed", false).unwrap(),
        first
    );
    assert_eq!(
        merge_hook(first, "claude", "managed", true).unwrap(),
        original
    );
}
#[test]
fn agy_named_hook_collision_is_not_overwritten() {
    assert!(
        merge_hook(
            json!({"ghostback":{"SessionStart":[]}}),
            "agy",
            "managed",
            false
        )
        .is_err()
    );
}
#[test]
fn uninstall_preserves_a_replaced_agy_hook() {
    let value =
        json!({"ghostback":{"description":"someone else","SessionStart":[{"command":"other"}]}});
    assert_eq!(
        merge_hook(value.clone(), "agy", "managed", true).unwrap(),
        value
    );
}
#[test]
fn malformed_hook_shapes_are_rejected_before_install() {
    for value in [
        json!([]),
        json!({"hooks":[]}),
        json!({"hooks":{"SessionStart":{}}}),
    ] {
        assert!(merge_hook(value, "codex", "managed", false).is_err());
    }
}
#[test]
fn dry_run_creates_nothing() {
    let (_root, mut options) = fixture();
    options.dry_run = true;
    assert!(!install::setup(&options).unwrap().is_empty());
    assert!(!options.state.exists());
    assert!(!options.home.join(".zshrc").exists());
}
#[test]
fn setup_is_idempotent_and_uninstall_preserves_user_changes() {
    let (_root, options) = fixture();
    let zsh = options.home.join(".zshrc");
    fs::write(&zsh, "export USER_SETTING=before\n").unwrap();
    install::setup(&options).unwrap();
    let first = fs::read(&zsh).unwrap();
    install::setup(&options).unwrap();
    assert_eq!(fs::read(&zsh).unwrap(), first);
    let changed = String::from_utf8(first).unwrap() + "export USER_ADDED=after\n";
    fs::write(&zsh, changed).unwrap();
    install::uninstall(&options.state, false).unwrap();
    let remaining = fs::read_to_string(zsh).unwrap();
    assert!(remaining.contains("USER_SETTING=before"));
    assert!(remaining.contains("USER_ADDED=after"));
    assert!(!remaining.contains("ghostback"));
    assert!(
        install::uninstall(&options.state, false)
            .unwrap()
            .is_empty()
    );
}
#[test]
fn broken_json_aborts_before_any_configuration_changes() {
    let (_root, options) = fixture();
    let path = options.home.join(".claude/settings.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"broken").unwrap();
    assert!(install::setup(&options).is_err());
    assert!(!options.home.join(".zshrc").exists());
    assert_eq!(fs::read(path).unwrap(), b"broken");
}
#[test]
fn unowned_binary_cannot_be_overwritten() {
    let (_root, options) = fixture();
    let target = options.home.join(".local/bin/ghostback");
    fs::create_dir_all(target.parent().unwrap()).unwrap();
    fs::write(&target, b"unowned").unwrap();
    assert!(install::setup(&options).is_err());
    assert_eq!(fs::read(target).unwrap(), b"unowned");
}
#[test]
fn modified_owned_binary_survives_uninstall() {
    let (_root, options) = fixture();
    install::setup(&options).unwrap();
    let target = options.home.join(".local/bin/ghostback");
    fs::write(&target, b"user modified").unwrap();
    install::uninstall(&options.state, false).unwrap();
    assert_eq!(fs::read(target).unwrap(), b"user modified");
}
#[test]
fn malformed_configuration_blocks_uninstall_without_partial_removal() {
    let (_root, options) = fixture();
    install::setup(&options).unwrap();
    let zsh = options.home.join(".zshrc");
    let original = fs::read(&zsh).unwrap();
    fs::write(options.home.join(".claude/settings.json"), b"broken").unwrap();
    assert!(install::uninstall(&options.state, false).is_err());
    assert_eq!(fs::read(zsh).unwrap(), original);
}
#[test]
fn generated_shell_code_parses_in_bash_and_zsh() {
    let (_root, options) = fixture();
    for (shell, bash) in [("/bin/bash", true), ("/bin/zsh", false)] {
        if !std::path::Path::new(shell).exists() {
            continue;
        }
        let text = install::shell_body(
            &options.home.join("bin with ' quote/ghostback"),
            &options.state,
            bash,
        );
        let file = options
            .home
            .join(if bash { "bash-test" } else { "zsh-test" });
        fs::write(&file, text).unwrap();
        assert!(
            Command::new(shell)
                .arg("-n")
                .arg(file)
                .status()
                .unwrap()
                .success()
        );
    }
}
#[cfg(unix)]
#[test]
fn symlinked_configuration_is_not_replaced() {
    use std::os::unix::fs::symlink;
    let (_root, options) = fixture();
    let target = options.home.join("real-zsh");
    fs::write(&target, b"original").unwrap();
    symlink(&target, options.home.join(".zshrc")).unwrap();
    assert!(install::setup(&options).is_err());
    assert_eq!(fs::read(target).unwrap(), b"original");
}
#[test]
fn atomic_writes_leave_complete_content() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("state/config");
    files::atomic_write(&path, b"first").unwrap();
    files::atomic_write(&path, b"second").unwrap();
    assert_eq!(fs::read(path).unwrap(), b"second");
}
#[cfg(unix)]
#[test]
fn helper_lock_is_exclusive_and_released_on_drop() {
    let root = TempDir::new().unwrap();
    let path = root.path().join("lock");
    let guard = files::Lock::acquire(&path).unwrap();
    assert!(files::Lock::acquire(&path).is_err());
    drop(guard);
    assert!(files::Lock::acquire(&path).is_ok());
}

#[test]
fn interrupted_setup_rolls_back_only_its_unchanged_writes() {
    let (_root, options) = fixture();
    fs::create_dir_all(options.state.join("setup-backup/test")).unwrap();
    let original = options.home.join("original");
    let created = options.home.join("created");
    let edited = options.home.join("user-edited");
    let backup = options.state.join("setup-backup/test/0");
    fs::write(&backup, b"before").unwrap();
    fs::write(&original, b"after").unwrap();
    fs::write(&created, b"after").unwrap();
    fs::write(&edited, b"concurrent user edit").unwrap();
    let journal = json!({"home":options.home,"manifest_digest":"uncommitted","entries":[
        {"path":original,"backup":backup,"after_digest":install::digest(b"after"),"mode":493},
        {"path":created,"backup":null,"after_digest":install::digest(b"after")},
        {"path":edited,"backup":null,"after_digest":install::digest(b"after")}
    ]});
    fs::write(
        options.state.join("setup-pending.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    install::recover(&options.state, &options.home).unwrap();
    assert_eq!(fs::read(&original).unwrap(), b"before");
    assert!(!created.exists());
    assert_eq!(fs::read(edited).unwrap(), b"concurrent user edit");
    assert!(!options.state.join("setup-pending.json").exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(original).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }
}
#[test]
fn committed_setup_with_leftover_journal_is_not_rolled_back() {
    let (_root, options) = fixture();
    fs::create_dir_all(&options.state).unwrap();
    let target = options.home.join("installed");
    fs::write(&target, b"after").unwrap();
    fs::write(options.state.join("install.json"), b"committed manifest").unwrap();
    let journal = json!({"home":options.home,"manifest_digest":install::digest(b"committed manifest"),"entries":[
        {"path":target,"backup":null,"after_digest":install::digest(b"after")}
    ]});
    fs::write(
        options.state.join("setup-pending.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    install::recover(&options.state, &options.home).unwrap();
    assert_eq!(fs::read(target).unwrap(), b"after");
}
#[test]
fn invalid_journal_is_rejected_before_rollback_changes() {
    let (_root, options) = fixture();
    fs::create_dir_all(&options.state).unwrap();
    let target = options.home.join("installed");
    fs::write(&target, b"after").unwrap();
    let journal = json!({"home":options.home,"manifest_digest":"uncommitted","entries":[
        {"path":target,"backup":options.state.join("setup-backup/../../outside"),"after_digest":install::digest(b"after")},
        {"path":target,"backup":null,"after_digest":install::digest(b"after")}
    ]});
    fs::write(
        options.state.join("setup-pending.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    assert!(install::recover(&options.state, &options.home).is_err());
    assert_eq!(fs::read(target).unwrap(), b"after");
}

#[test]
fn bash_prompt_integration_preserves_a_trailing_semicolon() {
    let (_root, options) = fixture();
    let file = options.home.join("test-bashrc");
    let body = install::shell_body(&options.source, &options.state, true);
    fs::write(
        &file,
        format!("PROMPT_COMMAND='printf original; '\n{body}\neval \"$PROMPT_COMMAND\"\n"),
    )
    .unwrap();
    let result = Command::new("/bin/bash")
        .arg(&file)
        .env_remove("GHOSTTY_SURFACE_ID")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(result.stdout, b"original");
}

#[cfg(unix)]
fn brew_binary(options: &Options, version: &str) -> std::path::PathBuf {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let prefix = options.home.parent().unwrap().join("brew");
    let keg = prefix.join("Cellar/ghostback").join(version);
    fs::create_dir_all(keg.join("bin")).unwrap();
    let binary = keg.join("bin/ghostback");
    fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let stable = prefix.join("opt/ghostback");
    fs::create_dir_all(stable.parent().unwrap()).unwrap();
    if stable.symlink_metadata().is_ok() {
        fs::remove_file(&stable).unwrap();
    }
    symlink(&keg, stable).unwrap();
    binary.canonicalize().unwrap()
}

#[cfg(unix)]
#[test]
fn homebrew_setup_uses_stable_path_and_uninstall_preserves_managed_binary() {
    use ghostback::distribution::Distribution;
    let (_root, mut options) = fixture();
    options.source = brew_binary(&options, "0.1.0");
    install::setup(&options).unwrap();
    let manifest: install::Manifest =
        serde_json::from_slice(&fs::read(options.state.join("install.json")).unwrap()).unwrap();
    assert_eq!(manifest.distribution, Distribution::Homebrew);
    assert!(manifest.binary.ends_with("opt/ghostback/bin/ghostback"));
    assert!(!options.home.join(".local/bin/ghostback").exists());
    assert!(
        fs::read_to_string(options.home.join(".zshrc"))
            .unwrap()
            .contains(manifest.binary.to_str().unwrap())
    );
    assert!(!manifest.files.iter().any(|f| f.path == manifest.binary));
    install::uninstall(&options.state, false).unwrap();
    assert!(options.source.is_file());
    assert!(manifest.binary.is_file());
}

#[cfg(unix)]
#[test]
fn broken_homebrew_link_fails_before_setup_writes() {
    let (_root, mut options) = fixture();
    options.source = brew_binary(&options, "0.1.0");
    let prefix = options.source.ancestors().nth(5).unwrap();
    fs::remove_file(prefix.join("opt/ghostback")).unwrap();
    assert!(install::setup(&options).is_err());
    assert!(!options.state.exists());
    assert!(!options.home.join(".zshrc").exists());
}

#[cfg(unix)]
#[test]
fn watcher_detects_homebrew_upgrade_through_stable_link() {
    let (_root, mut options) = fixture();
    let old = brew_binary(&options, "0.1.0");
    options.source = old.clone();
    install::setup(&options).unwrap();
    assert!(
        install::homebrew_upgrade(&options.state, &old)
            .unwrap()
            .is_none()
    );
    let new = brew_binary(&options, "0.2.0");
    let next = install::homebrew_upgrade(&options.state, &old)
        .unwrap()
        .unwrap();
    assert_eq!(next.canonicalize().unwrap(), new);
    assert!(
        install::homebrew_upgrade(&options.state, &new)
            .unwrap()
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn standalone_migration_replaces_hooks_and_removes_only_owned_copy() {
    let (_root, mut options) = fixture();
    install::setup(&options).unwrap();
    let copied = options.home.join(".local/bin/ghostback");
    assert!(copied.is_file());
    options.source = brew_binary(&options, "0.1.0");
    install::setup(&options).unwrap();
    assert!(!copied.exists());
    let hooks = fs::read_to_string(options.home.join(".claude/settings.json")).unwrap();
    assert!(!hooks.contains(copied.to_str().unwrap()));
    assert_eq!(hooks.matches("hook claude").count(), 2);
    install::setup(&options).unwrap();
    assert_eq!(
        fs::read_to_string(options.home.join(".claude/settings.json")).unwrap(),
        hooks
    );
    install::uninstall(&options.state, false).unwrap();
    assert!(options.source.is_file());
}

#[cfg(unix)]
#[test]
fn migration_preserves_a_user_modified_old_copy() {
    let (_root, mut options) = fixture();
    install::setup(&options).unwrap();
    let copied = options.home.join(".local/bin/ghostback");
    fs::write(&copied, b"user replacement").unwrap();
    options.source = brew_binary(&options, "0.1.0");
    install::setup(&options).unwrap();
    assert_eq!(fs::read(copied).unwrap(), b"user replacement");
}

#[test]
fn rollback_restores_an_owned_file_deleted_during_migration() {
    let (_root, options) = fixture();
    fs::create_dir_all(options.state.join("setup-backup/migration")).unwrap();
    let target = options.home.join("old-binary");
    let backup = options.state.join("setup-backup/migration/0");
    fs::write(&backup, b"old binary").unwrap();
    let journal = json!({"home":options.home,"manifest_digest":"uncommitted","entries":[{"path":target,"backup":backup,"after_digest":null,"mode":493}]});
    fs::write(
        options.state.join("setup-pending.json"),
        serde_json::to_vec(&journal).unwrap(),
    )
    .unwrap();
    install::recover(&options.state, &options.home).unwrap();
    assert_eq!(fs::read(target).unwrap(), b"old binary");
}

#[test]
fn old_manifest_without_distribution_still_supports_setup() {
    let (_root, options) = fixture();
    install::setup(&options).unwrap();
    let path = options.state.join("install.json");
    let mut value: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("distribution");
    fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    install::setup(&options).unwrap();
    assert!(options.home.join(".local/bin/ghostback").is_file());
}
