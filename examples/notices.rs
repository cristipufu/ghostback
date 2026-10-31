//! Release-time license collection; this program is never shipped in the binary.
use ghostback::{Result, invalid};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn run() -> Result<()> {
    let destination = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("usage: cargo run --example notices -- <destination>"))?;
    let compiler = Command::new("rustc").arg("-vV").output()?;
    if !compiler.status.success() {
        return Err(invalid("could not read Rust host"));
    }
    let compiler = String::from_utf8(compiler.stdout)?;
    let host = compiler
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| invalid("missing Rust host"))?;
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--offline",
            "--locked",
            "--format-version",
            "1",
            "--filter-platform",
            host,
        ])
        .output()?;
    if !output.status.success() {
        return Err(invalid(String::from_utf8_lossy(&output.stderr)));
    }
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let resolve = &metadata["resolve"];
    let root = resolve["root"]
        .as_str()
        .ok_or_else(|| invalid("missing root package"))?;
    let nodes = resolve["nodes"]
        .as_array()
        .ok_or_else(|| invalid("missing dependency graph"))?;
    let mut pending = vec![root.to_owned()];
    let mut used = HashSet::new();
    while let Some(id) = pending.pop() {
        if !used.insert(id.clone()) {
            continue;
        }
        let node = nodes
            .iter()
            .find(|node| node["id"].as_str() == Some(&id))
            .ok_or_else(|| invalid("missing dependency node"))?;
        for dependency in node["deps"]
            .as_array()
            .ok_or_else(|| invalid("invalid dependency node"))?
        {
            if dependency["dep_kinds"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|k| k["kind"].as_str() != Some("dev")))
            {
                pending.push(
                    dependency["pkg"]
                        .as_str()
                        .ok_or_else(|| invalid("invalid dependency ID"))?
                        .to_owned(),
                );
            }
        }
    }
    let mut sections = BTreeMap::new();
    for package in metadata["packages"]
        .as_array()
        .ok_or_else(|| invalid("missing package metadata"))?
    {
        let id = package["id"]
            .as_str()
            .ok_or_else(|| invalid("missing package ID"))?;
        if id == root || !used.contains(id) {
            continue;
        }
        let name = package["name"]
            .as_str()
            .ok_or_else(|| invalid("missing package name"))?;
        let version = package["version"]
            .as_str()
            .ok_or_else(|| invalid("missing package version"))?;
        let license = package["license"].as_str().unwrap_or("See license file");
        let manifest = PathBuf::from(
            package["manifest_path"]
                .as_str()
                .ok_or_else(|| invalid("missing manifest path"))?,
        );
        let directory = manifest
            .parent()
            .ok_or_else(|| invalid("invalid manifest path"))?;
        let mut paths = Vec::new();
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let title = entry.file_name().to_string_lossy().to_uppercase();
            if entry.file_type()?.is_file()
                && (title.starts_with("LICENSE")
                    || title.starts_with("COPYING")
                    || title.starts_with("COPYRIGHT")
                    || title == "NOTICE")
            {
                paths.push(entry.path());
            }
        }
        if let Some(file) = package["license_file"].as_str() {
            paths.push(directory.join(file));
        }
        paths.sort();
        paths.dedup();
        if paths.is_empty() {
            return Err(invalid(format!(
                "missing license text for {name} {version}"
            )));
        }
        let mut section = format!("\n\n===== {name} {version} ({license}) =====\n");
        for path in paths {
            section.push_str(&format!(
                "\n--- {} ---\n",
                path.file_name().unwrap_or_default().to_string_lossy()
            ));
            section.push_str(&String::from_utf8(fs::read(path)?)?);
        }
        sections.insert(format!("{name} {version}"), section);
    }
    let mut notices = "Ghostback third-party notices\n\nIncludes normal and build dependencies for this architecture.\nSQLite is compiled into Ghostback; SQLite is in the public domain.\n".to_owned();
    for section in sections.values() {
        notices.push_str(section);
    }
    fs::write(destination, notices)?;
    println!(
        "Collected license text for {} dependencies.",
        sections.len()
    );
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("notices: {error}");
        std::process::exit(1);
    }
}
