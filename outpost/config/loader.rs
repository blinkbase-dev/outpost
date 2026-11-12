use std::collections::BTreeMap;
use std::path::PathBuf;
use std::{fmt, fs, io};

use serde::Deserialize;

use super::locate::ConfigPaths;

pub(crate) fn load(paths: &ConfigPaths) -> Result<Vec<ConfigFile>, LoadError> {
	let mut files = Vec::new();
	for path in paths.system.iter().chain(&paths.project) {
		let content = match fs::read_to_string(path) {
			Ok(content) => content,
			Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
			Err(source) => return Err(LoadError::Io { path: path.clone(), source }),
		};
		let config = toml::from_str(&content).map_err(|source| LoadError::Parse { path: path.clone(), source })?;
		files.push(ConfigFile { path: path.clone(), config });
	}
	Ok(files)
}

pub(crate) struct ConfigFile {
	pub(crate) path: PathBuf,
	pub(crate) config: Config,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Config {
	#[serde(default)]
	pub(crate) tools: BTreeMap<String, ToolOptions>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum ToolOptions {
	Version(String),
	Table(ToolTable),
}

impl ToolOptions {
	pub(crate) fn version(&self) -> &str {
		match self {
			Self::Version(version) => version,
			Self::Table(table) => &table.version,
		}
	}
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ToolTable {
	version: String,
}

#[derive(Debug)]
pub(crate) enum LoadError {
	Io { path: PathBuf, source: io::Error },
	Parse { path: PathBuf, source: toml::de::Error },
}

impl fmt::Display for LoadError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => write!(f, "failed to read `{}`: {source}", path.display()),
			Self::Parse { path, source } => write!(f, "failed to parse `{}`: {source}", path.display()),
		}
	}
}

impl std::error::Error for LoadError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::Parse { source, .. } => Some(source),
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
		let files = load(&ConfigPaths {
			system: vec![path.clone()],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].path, path);
		assert_eq!(files[0].config.tools["github:owner/repo"], ToolOptions::Version("latest".to_owned()));
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
		let files = load(&ConfigPaths {
			system: vec![path],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(
			files[0].config.tools["github:owner/repo"],
			ToolOptions::Table(ToolTable { version: "0.26.0".to_owned() })
		);
	}

	#[test]
	fn keeps_files_separate() {
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
		let system_env = tmp.path().join("outpost.dev.toml");
		fs::write(
			&system_env,
			r#"
			[tools]
			"github:a/one" = "2"
			"#,
		)
		.unwrap();
		let project_outer = tmp.path().join("outer.toml");
		fs::write(
			&project_outer,
			r#"
			[tools]
			"github:a/one" = "3"
			"#,
		)
		.unwrap();
		let project_inner = tmp.path().join("inner.toml");
		fs::write(
			&project_inner,
			r#"
			[tools]
			"github:a/one" = "4"
			"github:a/three" = "3"
			"#,
		)
		.unwrap();
		let files = load(&ConfigPaths {
			system: vec![system, system_env],
			project: vec![project_outer, project_inner],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 4);
		assert_eq!(files[0].config.tools.len(), 2);
		assert_eq!(files[0].config.tools["github:a/one"].version(), "1");
		assert_eq!(files[0].config.tools["github:a/two"].version(), "1");
		assert_eq!(files[1].config.tools["github:a/one"].version(), "2");
		assert_eq!(files[2].config.tools["github:a/one"].version(), "3");
		assert_eq!(files[3].config.tools.len(), 2);
		assert_eq!(files[3].config.tools["github:a/one"].version(), "4");
		assert_eq!(files[3].config.tools["github:a/three"].version(), "3");
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
		let files = load(&ConfigPaths {
			system: vec![missing],
			project: vec![path.clone()],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].path, path);
		assert_eq!(files[0].config.tools.len(), 1);
		assert_eq!(files[0].config.tools["github:owner/repo"], ToolOptions::Version("latest".to_owned()));
	}

	#[test]
	fn defaults_without_files() {
		let files = load(&ConfigPaths {
			system: vec![],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert!(files.is_empty());
	}

	#[test]
	fn rejects_invalid_file() {
		let tmp = tempfile::tempdir().unwrap();
		let path = tmp.path().join("outpost.toml");
		fs::write(&path, "[tools\n").unwrap();
		let result = load(&ConfigPaths {
			system: vec![path.clone()],
			project: vec![],
			..ConfigPaths::default()
		});
		let error = result.err().expect("expected a parse error");
		assert!(matches!(&error, LoadError::Parse { .. }));
		assert!(error.to_string().contains(&path.display().to_string()));
	}
}
