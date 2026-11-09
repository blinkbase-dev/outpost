use std::collections::BTreeMap;
use std::{fmt, fs, io};

use serde::Deserialize;

use super::locate::ConfigPaths;

pub fn load(paths: &ConfigPaths) -> Result<Config, LoadError> {
	let mut config = Config::default();
	for path in paths.system.iter().chain(&paths.project) {
		let content = match fs::read_to_string(path) {
			Ok(content) => content,
			Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
			Err(source) => return Err(source.into()),
		};
		let outpost: Config = toml::from_str(&content)?;
		config.tools.extend(outpost.tools);
	}
	Ok(config)
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Config {
	#[serde(default)]
	pub tools: BTreeMap<String, ToolOptions>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum ToolOptions {
	Version(String),
	Table(ToolTable),
}

impl ToolOptions {
	pub fn version(&self) -> &str {
		match self {
			Self::Version(version) => version,
			Self::Table(table) => &table.version,
		}
	}
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ToolTable {
	version: String,
}

#[derive(Debug)]
pub enum LoadError {
	Io(io::Error),
	Parse(toml::de::Error),
}

impl From<io::Error> for LoadError {
	fn from(error: io::Error) -> Self {
		Self::Io(error)
	}
}

impl From<toml::de::Error> for LoadError {
	fn from(error: toml::de::Error) -> Self {
		Self::Parse(error)
	}
}

impl fmt::Display for LoadError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(f, "{error}"),
			Self::Parse(error) => write!(f, "{error}"),
		}
	}
}

impl std::error::Error for LoadError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::Parse(error) => Some(error),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn loads_string_form() {
		let tmp = tempfile::tempdir().unwrap();
		let path = tmp.path().join("outpost.toml");
		fs::write(
			&path,
			r#"
			[tools]
			"github:owner/repo" = "latest"
			"#,
		)
		.unwrap();
		let config = load(&ConfigPaths {
			system: vec![path],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools["github:owner/repo"], ToolOptions::Version("latest".to_owned()));
	}

	#[test]
	fn loads_table_form() {
		let tmp = tempfile::tempdir().unwrap();
		let path = tmp.path().join("outpost.toml");
		fs::write(
			&path,
			r#"
			[tools]
			"github:owner/repo" = { version = "0.26.0" }
			"#,
		)
		.unwrap();
		let config = load(&ConfigPaths {
			system: vec![path],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(
			config.tools["github:owner/repo"],
			ToolOptions::Table(ToolTable { version: "0.26.0".to_owned() })
		);
	}

	#[test]
	fn tools_override() {
		let tmp = tempfile::tempdir().unwrap();
		let system = tmp.path().join("config.toml");
		fs::write(
			&system,
			r#"
			[tools]
			"github:owner/repo" = "1"
			"#,
		)
		.unwrap();
		let system_env = tmp.path().join("outpost.dev.toml");
		fs::write(
			&system_env,
			r#"
			[tools]
			"github:owner/repo" = "2"
			"#,
		)
		.unwrap();
		let project_outer = tmp.path().join("outer.toml");
		fs::write(
			&project_outer,
			r#"
			[tools]
			"github:owner/repo" = "3"
			"#,
		)
		.unwrap();
		let project_inner = tmp.path().join("inner.toml");
		fs::write(
			&project_inner,
			r#"
			[tools]
			"github:owner/repo" = "4"
			"#,
		)
		.unwrap();

		let config = load(&ConfigPaths {
			system: vec![system.clone()],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools["github:owner/repo"].version(), "1");

		let config = load(&ConfigPaths {
			system: vec![system.clone(), system_env.clone()],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools["github:owner/repo"].version(), "2");

		let config = load(&ConfigPaths {
			system: vec![system.clone(), system_env.clone()],
			project: vec![project_outer.clone()],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools["github:owner/repo"].version(), "3");

		let config = load(&ConfigPaths {
			system: vec![system, system_env],
			project: vec![project_outer, project_inner],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools["github:owner/repo"].version(), "4");
	}

	#[test]
	fn tools_merge() {
		let tmp = tempfile::tempdir().unwrap();
		let system = tmp.path().join("config.toml");
		fs::write(
			&system,
			r#"
			[tools]
			"github:a/one" = "1"
			"github:a/two" = "1"
			"#,
		)
		.unwrap();
		let project = tmp.path().join("outpost.toml");
		fs::write(
			&project,
			r#"
			[tools]
			"github:a/one" = "2"
			"github:a/three" = "3"
			"#,
		)
		.unwrap();
		let config = load(&ConfigPaths {
			system: vec![system],
			project: vec![project],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools.len(), 3);
		assert_eq!(config.tools["github:a/one"].version(), "2");
		assert_eq!(config.tools["github:a/two"].version(), "1");
		assert_eq!(config.tools["github:a/three"].version(), "3");
	}

	#[test]
	fn skips_missing_files() {
		let tmp = tempfile::tempdir().unwrap();
		let missing = tmp.path().join("missing.toml");
		let path = tmp.path().join("outpost.toml");
		fs::write(
			&path,
			r#"
			[tools]
			"github:owner/repo" = "latest"
			"#,
		)
		.unwrap();
		let config = load(&ConfigPaths {
			system: vec![missing],
			project: vec![path],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(config.tools.len(), 1);
		assert_eq!(config.tools["github:owner/repo"], ToolOptions::Version("latest".to_owned()));
	}

	#[test]
	fn defaults_without_files() {
		let config = load(&ConfigPaths {
			system: vec![],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert!(config.tools.is_empty());
	}

	#[test]
	fn rejects_invalid_file() {
		let tmp = tempfile::tempdir().unwrap();
		let path = tmp.path().join("outpost.toml");
		fs::write(&path, "[tools\n").unwrap();
		let result = load(&ConfigPaths {
			system: vec![path],
			project: vec![],
			..ConfigPaths::default()
		});
		assert!(matches!(result, Err(LoadError::Parse(_))));
	}
}
