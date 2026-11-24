//! Read-only Oh My Pi 19.1.1 path and MCP registration inspection.

use std::ffi::{OsStr, OsString};
use std::fs::{self, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::fs_identity::is_link_or_reparse_point;

pub(super) fn active_agent_dir() -> io::Result<Option<PathBuf>> {
    // OMP uses os.homedir(): USERPROFILE on Windows, HOME on Unix.
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    resolve_agent_dir(home.as_deref(), |key| std::env::var_os(key))
}

fn profile(value: Option<OsString>) -> io::Result<Option<String>> {
    let Some(value) = value else { return Ok(None) };
    let name = value.to_str().unwrap_or("�").trim();
    if name.is_empty() || name == "default" {
        return Ok(None);
    }
    let alnum = |b: u8| b.is_ascii_lowercase() || b.is_ascii_digit();
    let base = name.split('.').next().unwrap_or_default();
    let device = matches!(base, "con" | "prn" | "aux" | "nul")
        || (base.len() == 4
            && (base.starts_with("com") || base.starts_with("lpt"))
            && base.as_bytes()[3].is_ascii_digit());
    if name.len() > 64
        || !alnum(name.as_bytes()[0])
        || !name.bytes().all(|b| alnum(b) || b"._-".contains(&b))
        || name.ends_with('.')
        || device
    {
        return Err(invalid("invalid OMP profile name"));
    }
    Ok(Some(name.to_owned()))
}

fn resolve_agent_dir(
    home: Option<&Path>,
    env: impl Fn(&str) -> Option<OsString>,
) -> io::Result<Option<PathBuf>> {
    let legacy = env("PI_PROFILE");
    let selected = profile(env("OMP_PROFILE").or_else(|| legacy.clone()))?;
    // PI_CONFIG_DIR is a directory name joined to home, not an agent override.
    let config = env("PI_CONFIG_DIR").filter(|v| !v.is_empty());
    let mut config_dir = PathBuf::new();
    for part in Path::new(config.as_deref().unwrap_or(OsStr::new(".omp"))).components() {
        match part {
            Component::Normal(name) => config_dir.push(name),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir if config_dir.pop() => {}
            _ => {
                return Err(invalid(
                    "PI_CONFIG_DIR must stay inside the OMP home directory",
                ));
            }
        }
    }
    if selected.is_none()
        && let Some(path) = env("PI_CODING_AGENT_DIR").filter(|v| !v.is_empty())
    {
        let path = std::path::absolute(path)?;
        // OMP exports the derived directory to child processes. In default
        // mode do not inherit a bypassed PI_PROFILE's derived override.
        let inherited = home
            .zip(profile(legacy).ok().flatten())
            .is_some_and(|(home, name)| {
                path == home
                    .join(&config_dir)
                    .join("profiles")
                    .join(name)
                    .join("agent")
            });
        if !inherited {
            return existing_dir(&path);
        }
    }
    let Some(home) = home else { return Ok(None) };
    if let Some(name) = selected {
        config_dir = config_dir.join("profiles").join(name);
    }
    let relative = config_dir.join("agent");
    if direct_metadata(home, &relative)?.is_none() {
        return Ok(None);
    }
    existing_dir(&home.join(relative))
}

fn existing_dir(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() && !is_link_or_reparse_point(&meta) => {
            fs::canonicalize(path).map(Some)
        }
        Ok(_) => Err(invalid("OMP agent path is not a direct directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

/// Inspect fixed descendants without following a redirect to another profile
/// (or outside the project). The caller has selected the root itself.
pub(super) fn direct_metadata(root: &Path, relative: &Path) -> io::Result<Option<Metadata>> {
    let mut path = root.to_path_buf();
    let mut parts = relative.components().peekable();
    while let Some(Component::Normal(name)) = parts.next() {
        path.push(name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if is_link_or_reparse_point(&meta) {
            return Err(invalid("linked OMP path is not inspected"));
        }
        if parts.peek().is_none() {
            return Ok(Some(meta));
        }
        if !meta.is_dir() {
            return Err(invalid("OMP path has a non-directory parent"));
        }
    }
    Err(invalid("expected a relative OMP descendant path"))
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Registration {
    Missing,
    Malformed(PathBuf),
    Disabled(PathBuf),
    Configured(PathBuf),
}

fn read_mcp(root: &Path, relative: &str) -> io::Result<Option<Value>> {
    let relative = Path::new(relative);
    let Some(meta) = direct_metadata(root, relative)? else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Err(invalid("not an MCP config file"));
    }
    let bytes = fs::read(root.join(relative))?;
    let value: Value = serde_json::from_slice(&bytes)?;
    if !value.is_object() || value.get("mcpServers").is_some_and(|map| !map.is_object()) {
        return Err(invalid("invalid MCP server map"));
    }
    Ok(Some(value))
}

fn contains_memhub(list: Option<&Value>) -> bool {
    list.and_then(Value::as_array)
        .is_some_and(|names| names.iter().any(|name| name.as_str() == Some("memhub")))
}

fn entry_enabled(entry: &Value, native: bool) -> bool {
    match &entry["enabled"] {
        Value::Bool(enabled) => *enabled,
        Value::String(value) if native => !(value.eq_ignore_ascii_case("false") || value == "0"),
        _ => true, // OMP ignores other values, including strings in root fallback files.
    }
}

fn valid_endpoint(entry: &Value) -> bool {
    let nonempty = |key| {
        entry
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    let command = nonempty("command");
    let url = nonempty("url");
    if command == url
        || entry.get("args").is_some_and(|args| {
            !args
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string))
        })
    {
        return false;
    }
    match entry.get("type") {
        Some(Value::String(kind)) if kind == "stdio" => command,
        Some(Value::String(kind)) if matches!(kind.as_str(), "http" | "sse") => url,
        None => true,
        Some(_) => false,
    }
}

/// Scope and order match OMP 19.1.1's native provider and standalone fallback.
/// A file that contributes no entries does not shadow subsequent sources.
pub(super) fn registration(cwd: &Path, agent: &Path) -> (Registration, Vec<PathBuf>) {
    let user = read_mcp(agent, "mcp.json");
    let list = |key| {
        user.as_ref()
            .ok()
            .and_then(Option::as_ref)
            .and_then(|v| v.get(key))
    };
    let deny = contains_memhub(list("disabledServers"));
    let force = contains_memhub(list("enabledServers"));
    let mut malformed = Vec::new();
    if user.is_err() {
        malformed.push(agent.join("mcp.json"));
    }
    for (root, relative, native) in [
        (cwd, ".omp/mcp.json", true),
        (cwd, ".omp/.mcp.json", true),
        (agent, "mcp.json", true),
        (agent, ".mcp.json", true),
        (cwd, "mcp.json", false),
        (cwd, ".mcp.json", false),
    ] {
        let path = root.join(relative);
        let config = if root == agent && relative == "mcp.json" {
            // Already read for the profile lists.
            let Ok(config) = &user else { continue };
            config.clone()
        } else {
            match read_mcp(root, relative) {
                Ok(config) => config,
                Err(_) => {
                    malformed.push(path);
                    continue;
                }
            }
        };
        if let Some(entry) = config.as_ref().and_then(|v| v["mcpServers"].get("memhub")) {
            let result = if !entry.is_object() {
                Registration::Malformed(path)
            } else if deny || (!force && !entry_enabled(entry, native)) {
                Registration::Disabled(path)
            } else if valid_endpoint(entry) {
                Registration::Configured(path)
            } else {
                Registration::Malformed(path)
            };
            return (result, malformed);
        }
    }
    let result = match malformed.first() {
        Some(path) => Registration::Malformed(path.clone()),
        None if deny => Registration::Disabled(agent.join("mcp.json")),
        None => Registration::Missing,
    };
    (result, malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omp_agent_directory_uses_only_selected_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let make = |relative: &str| {
            let dir = home.join(relative);
            fs::create_dir_all(&dir).unwrap();
            fs::canonicalize(dir).unwrap()
        };
        let default = make(".omp/agent");
        let profile = make(".custom/profiles/work.1/agent");
        let custom_default = make(".custom/agent");
        let override_dir = make("override");
        let resolve = |vars: &[(&str, &OsStr)]| {
            resolve_agent_dir(Some(home), |key| {
                vars.iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| v.to_os_string())
            })
        };
        assert_eq!(resolve(&[]).unwrap(), Some(default));
        let cfg = ("PI_CONFIG_DIR", OsStr::new(".custom"));
        let legacy = ("PI_PROFILE", OsStr::new("work.1"));
        let agent = ("PI_CODING_AGENT_DIR", override_dir.as_os_str());
        assert_eq!(resolve(&[cfg, legacy, agent]).unwrap(), Some(profile));
        assert_eq!(
            resolve(&[cfg, legacy, ("OMP_PROFILE", OsStr::new("missing")), agent]).unwrap(),
            None
        );
        assert_eq!(
            resolve(&[cfg, legacy, ("OMP_PROFILE", OsStr::new("default")), agent]).unwrap(),
            Some(override_dir)
        );
        let derived = home.join(".custom/profiles/work.1/agent");
        assert_eq!(
            resolve(&[
                cfg,
                legacy,
                ("OMP_PROFILE", OsStr::new("")),
                ("PI_CODING_AGENT_DIR", derived.as_os_str())
            ])
            .unwrap(),
            Some(custom_default)
        );
        for invalid in ["../other", "CON.file", "trailing.", "MixedCase", ".", "a/b"] {
            assert!(
                resolve(&[("OMP_PROFILE", OsStr::new(invalid))]).is_err(),
                "{invalid}"
            );
        }
    }

    #[test]
    fn registration_precedence_toggles_and_malformed_sources() {
        let temp = tempfile::tempdir().unwrap();
        let cwd = temp.path().join("repo");
        let agent = temp.path().join("profile/agent");
        fs::create_dir_all(cwd.join(".omp")).unwrap();
        fs::create_dir_all(&agent).unwrap();
        let check = || registration(&cwd, &agent);
        assert_eq!(check().0, Registration::Missing);
        let active = r#"{"mcpServers":{"memhub":{"command":"memhub","args":["serve"]}}}"#;
        let targets = [
            cwd.join(".omp/mcp.json"),
            cwd.join(".omp/.mcp.json"),
            agent.join("mcp.json"),
            agent.join(".mcp.json"),
            cwd.join("mcp.json"),
            cwd.join(".mcp.json"),
        ];
        for path in targets.iter().rev() {
            fs::write(path, active).unwrap();
            assert_eq!(check().0, Registration::Configured(path.clone()));
        }
        fs::write(
            &targets[0],
            r#"{"mcpServers":{"memhub":{"enabled":"0","command":"memhub"}}}"#,
        )
        .unwrap();
        assert_eq!(check().0, Registration::Disabled(targets[0].clone()));
        fs::write(&targets[2], r#"{"enabledServers":["memhub"]}"#).unwrap();
        assert_eq!(check().0, Registration::Configured(targets[0].clone()));
        fs::write(
            &targets[2],
            r#"{"enabledServers":["memhub"],"disabledServers":["memhub"]}"#,
        )
        .unwrap();
        assert_eq!(check().0, Registration::Disabled(targets[0].clone()));
        fs::remove_file(&targets[2]).unwrap();
        fs::write(&targets[0], "{").unwrap();
        assert_eq!(
            check(),
            (
                Registration::Configured(targets[1].clone()),
                vec![targets[0].clone()]
            )
        );
        fs::write(
            &targets[1],
            r#"{"mcpServers":{"memhub":{"type":"stdio","url":"http://localhost"}}}"#,
        )
        .unwrap();
        assert_eq!(check().0, Registration::Malformed(targets[1].clone()));
        for path in &targets[3..] {
            fs::remove_file(path).unwrap();
        }
        fs::remove_file(&targets[1]).unwrap();
        assert_eq!(check().0, Registration::Malformed(targets[0].clone()));
    }
}
