use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use thiserror::Error;

const SKILL_FILE: &str = "SKILL.md";
const MAX_SKILL_NAME_LENGTH: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSkill {
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub path: PathBuf,
    pub instructions: Arc<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkspaceSkillMetadata {
    pub name: Arc<str>,
    pub description: Arc<str>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WorkspaceSkillCatalog {
    skills: BTreeMap<Arc<str>, WorkspaceSkill>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SkillCreation {
    pub name: Arc<str>,
    pub description: Arc<str>,
    pub resources: BTreeSet<SkillResourceKind>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum SkillResourceKind {
    Scripts,
    References,
    Assets,
}

#[derive(Debug, Error)]
pub enum SkillStoreError {
    #[error("skill store IO failed at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("skill validation failed at {path}: {message}")]
    Invalid { path: PathBuf, message: String },
    #[error("configured skill '{0}' was not found in workspace skills")]
    Missing(Arc<str>),
    #[error("skill '{0}' already exists")]
    Exists(Arc<str>),
}

impl WorkspaceSkillCatalog {
    pub fn load_enabled(root: &Path, enabled: &[Arc<str>]) -> Result<Self, SkillStoreError> {
        let discovered = discover_skills(root)?;
        if enabled.is_empty() {
            return Ok(Self { skills: discovered });
        }

        let mut skills = BTreeMap::new();
        for name in enabled {
            let normalized = normalize_skill_name(name);
            let Some(skill) = discovered.get::<str>(normalized.as_str()).cloned() else {
                return Err(SkillStoreError::Missing(Arc::clone(name)));
            };
            skills.insert(Arc::clone(&skill.name), skill);
        }
        Ok(Self { skills })
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }

    pub fn contains(&self, name: &str) -> bool {
        let normalized = normalize_skill_name(name);
        self.skills.contains_key::<str>(normalized.as_str())
    }

    pub fn metadata(&self) -> Vec<WorkspaceSkillMetadata> {
        self.skills
            .values()
            .map(|skill| WorkspaceSkillMetadata {
                name: Arc::clone(&skill.name),
                description: Arc::clone(&skill.description),
            })
            .collect()
    }

    pub fn skills(&self) -> impl Iterator<Item = &WorkspaceSkill> {
        self.skills.values()
    }
}

impl SkillCreation {
    pub fn new(name: impl AsRef<str>, description: impl Into<Arc<str>>) -> Self {
        Self {
            name: Arc::from(normalize_skill_name(name.as_ref())),
            description: description.into(),
            resources: BTreeSet::new(),
        }
    }

    pub fn with_resource(mut self, resource: SkillResourceKind) -> Self {
        self.resources.insert(resource);
        self
    }
}

pub fn create_skill(
    root: &Path,
    creation: &SkillCreation,
) -> Result<WorkspaceSkill, SkillStoreError> {
    validate_skill_name(&creation.name).map_err(|message| SkillStoreError::Invalid {
        path: root.join(creation.name.as_ref()),
        message,
    })?;
    if creation.description.trim().is_empty() {
        return Err(SkillStoreError::Invalid {
            path: root.join(creation.name.as_ref()),
            message: "description is required".to_owned(),
        });
    }

    fs::create_dir_all(root).map_err(|source| SkillStoreError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let skill_dir = root.join(creation.name.as_ref());
    if skill_dir.exists() {
        return Err(SkillStoreError::Exists(Arc::clone(&creation.name)));
    }
    fs::create_dir(&skill_dir).map_err(|source| SkillStoreError::Io {
        path: skill_dir.clone(),
        source,
    })?;

    for resource in &creation.resources {
        let name = match resource {
            SkillResourceKind::Scripts => "scripts",
            SkillResourceKind::References => "references",
            SkillResourceKind::Assets => "assets",
        };
        fs::create_dir(skill_dir.join(name)).map_err(|source| SkillStoreError::Io {
            path: skill_dir.join(name),
            source,
        })?;
    }

    let title = creation
        .name
        .split('-')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    let content = format!(
        "---\nname: {}\ndescription: {}\n---\n\n# {}\n\n## Workflow\n\nDescribe the repeatable workflow this skill standardizes. Keep the core instructions here and move detailed references, deterministic scripts, or reusable assets into bundled resource directories.\n\n## Validation\n\nRun `agentos skill validate {}` after editing this skill.\n",
        creation.name,
        yaml_scalar(&creation.description),
        title,
        creation.name
    );
    let skill_file = skill_dir.join(SKILL_FILE);
    fs::write(&skill_file, content).map_err(|source| SkillStoreError::Io {
        path: skill_file,
        source,
    })?;
    validate_skill_dir(&skill_dir)
}

pub fn validate_skill_dir(path: &Path) -> Result<WorkspaceSkill, SkillStoreError> {
    let skill_file = path.join(SKILL_FILE);
    let content = fs::read_to_string(&skill_file).map_err(|source| SkillStoreError::Io {
        path: skill_file.clone(),
        source,
    })?;
    let (frontmatter, instructions) =
        split_skill_markdown(&content).map_err(|message| SkillStoreError::Invalid {
            path: skill_file.clone(),
            message,
        })?;
    let metadata =
        parse_skill_frontmatter(frontmatter).map_err(|message| SkillStoreError::Invalid {
            path: skill_file.clone(),
            message,
        })?;
    let folder = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if metadata.name.as_ref() != folder {
        return Err(SkillStoreError::Invalid {
            path: skill_file,
            message: format!(
                "skill folder '{folder}' must match frontmatter name '{}'",
                metadata.name
            ),
        });
    }
    if instructions.trim().is_empty() {
        return Err(SkillStoreError::Invalid {
            path: skill_file,
            message: "SKILL.md body instructions are required".to_owned(),
        });
    }
    Ok(WorkspaceSkill {
        name: metadata.name,
        description: metadata.description,
        path: path.to_path_buf(),
        instructions: Arc::from(instructions.trim().to_owned()),
    })
}

fn discover_skills(root: &Path) -> Result<BTreeMap<Arc<str>, WorkspaceSkill>, SkillStoreError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(SkillStoreError::Io {
                path: root.to_path_buf(),
                source,
            })
        }
    };
    let mut skills = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| SkillStoreError::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill = validate_skill_dir(&path)?;
        skills.insert(Arc::clone(&skill.name), skill);
    }
    Ok(skills)
}

fn split_skill_markdown(content: &str) -> Result<(&str, &str), String> {
    let rest = content
        .strip_prefix("---\n")
        .ok_or_else(|| "SKILL.md must start with YAML frontmatter".to_owned())?;
    let Some(end) = rest.find("\n---") else {
        return Err("YAML frontmatter must end with --- delimiter".to_owned());
    };
    let frontmatter = &rest[..end];
    let instructions = rest[end + "\n---".len()..].trim_start_matches(['\r', '\n']);
    Ok((frontmatter, instructions))
}

fn parse_skill_frontmatter(input: &str) -> Result<WorkspaceSkillMetadata, String> {
    let mut name = None;
    let mut description = None;
    let mut nested_key: Option<&str> = None;
    for line in input.lines() {
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            if nested_key.is_some() {
                continue;
            }
            return Err("unexpected indented frontmatter line".to_owned());
        }
        nested_key = None;
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("invalid frontmatter line '{line}'"));
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "name" => {
                if value.is_empty() {
                    return Err("name must be a scalar value".to_owned());
                }
                let parsed = unquote_yaml_scalar(value);
                validate_skill_name(&parsed)?;
                name = Some(Arc::from(parsed));
            }
            "description" => {
                if value.is_empty() {
                    return Err("description must be a scalar value".to_owned());
                }
                let parsed = unquote_yaml_scalar(value);
                if parsed.trim().is_empty() {
                    return Err("description is required".to_owned());
                }
                if parsed.contains('<') || parsed.contains('>') {
                    return Err("description cannot contain angle brackets".to_owned());
                }
                if parsed.len() > 1024 {
                    return Err("description cannot exceed 1024 characters".to_owned());
                }
                description = Some(Arc::from(parsed));
            }
            "license" | "allowed-tools" | "metadata" => {
                if value.is_empty() {
                    nested_key = Some(key);
                }
            }
            other => {
                return Err(format!(
                    "unexpected frontmatter key '{other}'; expected name, description, license, allowed-tools, or metadata"
                ));
            }
        }
    }
    Ok(WorkspaceSkillMetadata {
        name: name.ok_or_else(|| "missing required name frontmatter".to_owned())?,
        description: description
            .ok_or_else(|| "missing required description frontmatter".to_owned())?,
    })
}

fn normalize_skill_name(input: impl AsRef<str>) -> String {
    let mut output = String::new();
    let mut previous_hyphen = false;
    for ch in input.as_ref().trim().chars() {
        if ch.is_ascii_alphanumeric() {
            output.push(ch.to_ascii_lowercase());
            previous_hyphen = false;
        } else if !previous_hyphen {
            output.push('-');
            previous_hyphen = true;
        }
    }
    output.trim_matches('-').to_owned()
}

fn validate_skill_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("name is required".to_owned());
    }
    if name.len() > MAX_SKILL_NAME_LENGTH {
        return Err(format!(
            "name cannot exceed {MAX_SKILL_NAME_LENGTH} characters"
        ));
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        return Err("name cannot start/end with hyphen or contain consecutive hyphens".to_owned());
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err("name must use lowercase letters, digits, and hyphens only".to_owned());
    }
    Ok(())
}

fn unquote_yaml_scalar(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.len() >= 2 {
        let first = trimmed.as_bytes()[0];
        let last = trimmed.as_bytes()[trimmed.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return trimmed[1..trimmed.len() - 1].to_owned();
        }
    }
    trimmed.to_owned()
}

fn yaml_scalar(input: &str) -> String {
    if input.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || ch.is_ascii_whitespace() || ".,;:!?()[]'/-".contains(ch)
    }) {
        input.to_owned()
    } else {
        format!("{:?}", input)
    }
}

