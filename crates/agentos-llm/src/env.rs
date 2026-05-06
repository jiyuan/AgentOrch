use std::env;
use std::fs;
use std::path::{Path, PathBuf};

pub const DEFAULT_ENV_PATH: &str = ".env";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EnvLoadOptions {
    pub explicit_path: Option<PathBuf>,
    pub search_parent_dirs: bool,
    pub allow_overrides: bool,
}

impl Default for EnvLoadOptions {
    fn default() -> Self {
        Self {
            explicit_path: None,
            search_parent_dirs: true,
            allow_overrides: true,
        }
    }
}

pub fn allow_env_overrides() -> bool {
    env::var("AGENTOS_NO_ENV_OVERRIDE").ok().as_deref() != Some("1")
}

pub fn load_startup_env(options: &EnvLoadOptions) -> Result<Option<PathBuf>, String> {
    let path = match &options.explicit_path {
        Some(path) => path.clone(),
        None => env::var_os("AGENTOS_ENV_FILE")
            .map(PathBuf::from)
            .map_or_else(|| find_default_env_file(options.search_parent_dirs), Ok)?,
    };
    if !path.is_file() {
        return Ok(None);
    }
    load_env_file(&path, options.allow_overrides)?;
    Ok(Some(path))
}

pub fn find_default_env_file(search_parent_dirs: bool) -> Result<PathBuf, String> {
    if !search_parent_dirs {
        return Ok(PathBuf::from(DEFAULT_ENV_PATH));
    }
    let mut dir = env::current_dir().map_err(|err| format!("failed to read current dir: {err}"))?;
    loop {
        let candidate = dir.join(DEFAULT_ENV_PATH);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if !dir.pop() {
            return Ok(PathBuf::from(DEFAULT_ENV_PATH));
        }
    }
}

pub fn load_env_file(path: &Path, allow_overrides: bool) -> Result<(), String> {
    let input = match fs::read_to_string(path) {
        Ok(input) => input,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(format!("failed to read env file {}: {err}", path.display())),
    };

    for (index, raw_line) in input.lines().enumerate() {
        let lineno = index + 1;
        let mut line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("export ") {
            line = rest.trim_start();
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "invalid .env entry at {}:{lineno}; use KEY=value syntax",
                path.display()
            ));
        };
        let key = key.trim();
        validate_env_key(key, path, lineno)?;
        if !allow_overrides && env::var_os(key).is_some() {
            continue;
        }
        env::set_var(
            key,
            parse_env_value(value.trim()).map_err(|err| {
                format!("invalid .env entry at {}:{lineno}; {err}", path.display())
            })?,
        );
    }
    Ok(())
}

fn validate_env_key(key: &str, path: &Path, lineno: usize) -> Result<(), String> {
    if key.is_empty() {
        return Err(format!("invalid .env key at {}:{lineno}", path.display()));
    }
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return Err(format!("invalid .env key at {}:{lineno}", path.display()));
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(format!("invalid .env key at {}:{lineno}", path.display()));
    }
    Ok(())
}

fn parse_env_value(value: &str) -> Result<String, String> {
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return Ok(value[1..value.len() - 1].to_owned());
    }
    if value.starts_with('"') || value.starts_with('\'') {
        return Err("unterminated quoted value".to_owned());
    }
    Ok(value.to_owned())
}
