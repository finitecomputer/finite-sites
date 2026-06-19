//! Project Config (`finite.toml`) parsing and validation.
//!
//! This module is shared by the CLI and server because `finite.toml` is the
//! contract agents read, write, commit, and push. The accepted schema is
//! intentionally narrower than TOML itself; unknown keys fail closed so agents
//! learn from deterministic errors instead of server inference.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::limits::{
    MAX_PROJECT_BRANCH_BYTES, MAX_PROJECT_OUTPUT_ID_BYTES, MAX_PROJECT_OUTPUT_PATH_BYTES,
    MAX_PROJECT_OUTPUTS, MAX_PROJECT_SLUG_BYTES,
};
use crate::{ProtoError, names};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub project: ProjectSection,
    #[serde(default)]
    pub outputs: BTreeMap<String, ProjectOutputConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectSection {
    pub slug: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectOutputConfig {
    pub kind: ProjectOutputKind,
    pub site_name: String,
    pub branch: String,
    pub path: String,
    #[serde(default)]
    pub spa: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectOutputKind {
    Site,
}

impl ProjectOutputKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProjectOutputKind::Site => "site",
        }
    }
}

impl ProjectConfig {
    pub fn validate(&self) -> Result<(), ProtoError> {
        validate_project_slug(&self.project.slug)?;
        if self.outputs.is_empty() {
            return Err(ProtoError::InvalidProjectConfig(
                "at least one output is required",
            ));
        }
        if self.outputs.len() > MAX_PROJECT_OUTPUTS as usize {
            return Err(ProtoError::InvalidProjectConfig("too many outputs"));
        }
        // Bounded by MAX_PROJECT_OUTPUTS above.
        for (output_id, output) in &self.outputs {
            validate_output_id(output_id)?;
            names::validate_site_name(&output.site_name)?;
            validate_branch_name(&output.branch)?;
            validate_output_path(&output.path)?;
        }
        Ok(())
    }

    pub fn to_toml_string(&self) -> Result<String, ProtoError> {
        self.validate()?;
        toml::to_string_pretty(self)
            .map_err(|_| ProtoError::InvalidProjectConfig("cannot encode toml"))
    }
}

pub fn parse_project_config_toml(input: &str) -> Result<ProjectConfig, ProtoError> {
    let config: ProjectConfig = toml::from_str(input)
        .map_err(|_| ProtoError::InvalidProjectConfig("toml does not match schema"))?;
    config.validate()?;
    Ok(config)
}

pub fn validate_project_slug(slug: &str) -> Result<(), ProtoError> {
    if slug.len() > MAX_PROJECT_SLUG_BYTES as usize {
        return Err(ProtoError::InvalidProjectConfig("project slug is too long"));
    }
    names::validate_site_name(slug).map_err(|_| {
        ProtoError::InvalidProjectConfig(
            "project slug must be a lowercase DNS label and not reserved",
        )
    })
}

fn validate_output_id(output_id: &str) -> Result<(), ProtoError> {
    if output_id.is_empty() {
        return Err(ProtoError::InvalidProjectConfig("output id is empty"));
    }
    if output_id.len() > MAX_PROJECT_OUTPUT_ID_BYTES as usize {
        return Err(ProtoError::InvalidProjectConfig("output id is too long"));
    }
    let bytes = output_id.as_bytes();
    let starts_valid = bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit();
    if !starts_valid {
        return Err(ProtoError::InvalidProjectConfig(
            "output id must start with lowercase letter or digit",
        ));
    }
    let all_valid = bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-' || *b == b'_');
    if !all_valid {
        return Err(ProtoError::InvalidProjectConfig(
            "output id may contain lowercase letters, digits, hyphen, and underscore",
        ));
    }
    Ok(())
}

fn validate_branch_name(branch: &str) -> Result<(), ProtoError> {
    if branch.is_empty() {
        return Err(ProtoError::InvalidProjectConfig("branch is empty"));
    }
    if branch.len() > MAX_PROJECT_BRANCH_BYTES as usize {
        return Err(ProtoError::InvalidProjectConfig("branch name is too long"));
    }
    if branch.starts_with('-')
        || branch.starts_with('/')
        || branch.ends_with('/')
        || branch.ends_with('.')
        || branch.ends_with(".lock")
        || branch.contains("..")
        || branch.contains("//")
    {
        return Err(ProtoError::InvalidProjectConfig(
            "branch name is not a safe deploy branch",
        ));
    }
    let all_valid = branch
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.'));
    if !all_valid {
        return Err(ProtoError::InvalidProjectConfig(
            "branch name contains unsupported characters",
        ));
    }
    Ok(())
}

fn validate_output_path(path: &str) -> Result<(), ProtoError> {
    if path.is_empty() {
        return Err(ProtoError::InvalidProjectConfig("output path is empty"));
    }
    if path.len() > MAX_PROJECT_OUTPUT_PATH_BYTES as usize {
        return Err(ProtoError::InvalidProjectConfig("output path is too long"));
    }
    if path == "." {
        return Ok(());
    }
    if path.starts_with('/') || path.ends_with('/') || path.contains('\\') {
        return Err(ProtoError::InvalidProjectConfig(
            "output path must be a relative directory path",
        ));
    }
    // Bounded by MAX_PROJECT_OUTPUT_PATH_BYTES.
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(ProtoError::InvalidProjectConfig(
                "output path contains an invalid component",
            ));
        }
        if matches!(component, ".git" | ".finite" | "node_modules") {
            return Err(ProtoError::InvalidProjectConfig(
                "output path targets a forbidden directory",
            ));
        }
        let all_safe = component
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
        if !all_safe {
            return Err(ProtoError::InvalidProjectConfig(
                "output path contains unsupported characters",
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> ProjectConfig {
        let mut outputs = BTreeMap::new();
        outputs.insert(
            "mockup".to_string(),
            ProjectOutputConfig {
                kind: ProjectOutputKind::Site,
                site_name: "finitechat-native-mockup".to_string(),
                branch: "main".to_string(),
                path: ".".to_string(),
                spa: false,
            },
        );
        ProjectConfig {
            project: ProjectSection {
                slug: "finitechat-native".to_string(),
            },
            outputs,
        }
    }

    #[test]
    fn parses_and_round_trips_minimal_schema() {
        let raw = r#"
[project]
slug = "finitechat-native"

[outputs.mockup]
kind = "site"
site_name = "finitechat-native-mockup"
branch = "main"
path = "."
spa = false
"#;
        let parsed = parse_project_config_toml(raw).unwrap();
        assert_eq!(parsed, valid_config());
        let encoded = parsed.to_toml_string().unwrap();
        assert!(encoded.contains("[project]"));
        assert!(encoded.contains("[outputs.mockup]"));
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        let unknown = r#"
[project]
slug = "finitechat-native"
extra = "nope"

[outputs.mockup]
kind = "site"
site_name = "finitechat-native-mockup"
branch = "main"
path = "."
"#;
        assert!(matches!(
            parse_project_config_toml(unknown),
            Err(ProtoError::InvalidProjectConfig(_))
        ));

        let mut config = valid_config();
        config.outputs.get_mut("mockup").unwrap().branch = "../main".to_string();
        assert_eq!(
            config.validate(),
            Err(ProtoError::InvalidProjectConfig(
                "branch name is not a safe deploy branch"
            ))
        );

        let mut config = valid_config();
        config.outputs.get_mut("mockup").unwrap().path = "node_modules".to_string();
        assert_eq!(
            config.validate(),
            Err(ProtoError::InvalidProjectConfig(
                "output path targets a forbidden directory"
            ))
        );
    }
}
