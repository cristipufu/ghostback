//! Executable ownership and stable Homebrew paths, independent of shell config.
use crate::{Result, app, invalid};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Distribution {
    #[default]
    Standalone,
    Homebrew,
}

pub struct Binary {
    pub path: PathBuf,
    pub distribution: Distribution,
}

pub fn installation_binary(source: &Path, home: &Path) -> Result<Binary> {
    let actual = source.canonicalize()?;
    let bin = actual.parent();
    let keg = bin.and_then(Path::parent);
    let rack = keg.and_then(Path::parent);
    let cellar = rack.and_then(Path::parent);
    if actual.file_name().is_some_and(|s| s == "ghostback")
        && bin.and_then(Path::file_name).is_some_and(|s| s == "bin")
        && rack
            .and_then(Path::file_name)
            .is_some_and(|s| s == "ghostback")
        && cellar
            .and_then(Path::file_name)
            .is_some_and(|s| s == "Cellar")
    {
        let prefix = cellar
            .and_then(Path::parent)
            .ok_or_else(|| invalid("invalid Homebrew prefix"))?;
        let path = prefix.join("opt/ghostback/bin/ghostback");
        if path.canonicalize().ok().as_ref() != Some(&actual) || !app::is_executable(&path) {
            return Err(invalid(
                "Homebrew's stable ghostback link is missing or points to another version. Run brew link ghostback, then retry setup.",
            ));
        }
        return Ok(Binary {
            path,
            distribution: Distribution::Homebrew,
        });
    }
    Ok(Binary {
        path: home.join(".local/bin/ghostback"),
        distribution: Distribution::Standalone,
    })
}
