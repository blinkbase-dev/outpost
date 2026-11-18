use std::collections::BTreeMap;
use std::path::PathBuf;
use std::{fmt, fs, io};

use serde::Deserialize;

use super::locate::ConfigPaths;

pub(crate) fn load(paths: &ConfigPaths) -> Result<Vec<ConfigFile>, LoadError> {
	// earlier files take precedence
	load_paths(paths.project.iter().chain(&paths.system).chain(&paths.state.registered_paths))
}

pub(crate) fn load_paths<'a>(paths: impl IntoIterator<Item = &'a PathBuf>) -> Result<Vec<ConfigFile>, LoadError> {
	let mut files = Vec::new();
	for path in paths {
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
#[serde(try_from = "RawConfig")]
pub(crate) struct Config {
	pub(crate) tools: BTreeMap<String, ToolOptions>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
	#[serde(default)]
	tools: BTreeMap<String, toml::Value>,
}

impl TryFrom<RawConfig> for Config {
	type Error = String;

	fn try_from(raw: RawConfig) -> Result<Self, Self::Error> {
		let mut tools = BTreeMap::new();
		for (key, value) in raw.tools {
			tools.insert(key.clone(), _tool_options(&key, value)?);
		}
		Ok(Self { tools })
	}
}

// the backend in the tool name picks the options table
fn _tool_options(key: &str, value: toml::Value) -> Result<ToolOptions, String> {
	let Some((backend, _)) = key.split_once(':') else {
		return Err(format!("missing backend in `{key}`"));
	};
	match backend {
		"github" => {
			let mut options: GithubOptionsTable = match value {
				// shorthand form fills the same table
				toml::Value::String(version) => GithubOptionsTable {
					version,
					api_url: None,
					exposed: None,
				},
				table => table.try_into().map_err(|error| format!("invalid options for `{key}`: {error}"))?,
			};
			// trim so the url can be concatenated with paths
			options.api_url = options.api_url.map(|api_url| api_url.trim().trim_end_matches('/').to_owned());
			// reject blank patterns since they cannot match a binary
			if let Some(exposed) = options.exposed.as_ref() {
				if exposed.iter().any(|pattern| pattern.trim().is_empty()) {
					return Err(format!("invalid `exposed` for `{key}`: expected non-empty patterns"));
				}
			}
			Ok(ToolOptions::Github(options))
		}
		_ => Err(format!("unsupported backend `{backend}` in `{key}`")),
	}
}

#[derive(Debug, PartialEq)]
pub(crate) enum ToolOptions {
	Github(GithubOptionsTable),
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub(crate) struct GithubOptionsTable {
	pub(crate) version: String,
	#[serde(default)]
	pub(crate) api_url: Option<String>,
	#[serde(default)]
	pub(crate) exposed: Option<Vec<String>>,
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
	use std::path::Path;

	use super::*;
	use crate::config::state::State;

	fn _config(dir: &Path, name: &str, tools: &str) -> PathBuf {
		let path = dir.join(name);
		fs::write(&path, format!("[tools]\n{tools}\n")).unwrap();
		path
	}

	fn _version(options: &ToolOptions) -> &str {
		match options {
			ToolOptions::Github(table) => &table.version,
		}
	}

	#[test]
	fn loads_files_from_all_sources() {
		let tmp = tempfile::tempdir().unwrap();
		let project = _config(tmp.path(), "project.toml", r#""github:a/project" = "1""#);
		let system = _config(tmp.path(), "system.toml", r#""github:a/system" = "2""#);
		let registered = _config(tmp.path(), "registered.toml", r#""github:a/registered" = "3""#);
		let files = load(&ConfigPaths {
			system: vec![system.clone()],
			project: vec![project.clone()],
			state: State {
				registered_paths: vec![registered.clone()],
				..State::default()
			},
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 3);
		assert_eq!(files[0].path, project);
		assert_eq!(files[1].path, system);
		assert_eq!(files[2].path, registered);
		assert_eq!(_version(&files[0].config.tools["github:a/project"]), "1");
		assert_eq!(_version(&files[1].config.tools["github:a/system"]), "2");
		assert_eq!(_version(&files[2].config.tools["github:a/registered"]), "3");
	}

	#[test]
	fn loads_string_form() {
		let tmp = tempfile::tempdir().unwrap();
		let path = _config(tmp.path(), "outpost.toml", r#""github:owner/repo" = "latest""#);
		let files = load(&ConfigPaths {
			system: vec![path.clone()],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].path, path);
		assert_eq!(
			files[0].config.tools["github:owner/repo"],
			ToolOptions::Github(GithubOptionsTable {
				version: "latest".to_owned(),
				api_url: None,
				exposed: None,
			})
		);
	}

	#[test]
	fn loads_table_form() {
		let tmp = tempfile::tempdir().unwrap();
		let tools = r#""github:owner/repo" = { version = "0.26.0" }
"github:owner/other" = { version = "0.26.0", api_url = " http://127.0.0.1:8080/ " }
"github:owner/empty" = { version = "0.26.0", exposed = [] }
"github:owner/exposed" = { version = "0.26.0", exposed = ["tool", "bin/*"] }"#;
		let path = _config(tmp.path(), "outpost.toml", tools);
		let files = load(&ConfigPaths {
			system: vec![path],
			project: vec![],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(
			files[0].config.tools["github:owner/repo"],
			ToolOptions::Github(GithubOptionsTable {
				version: "0.26.0".to_owned(),
				api_url: None,
				exposed: None,
			})
		);
		assert_eq!(
			files[0].config.tools["github:owner/other"],
			ToolOptions::Github(GithubOptionsTable {
				version: "0.26.0".to_owned(),
				api_url: Some("http://127.0.0.1:8080".to_owned()),
				exposed: None,
			})
		);
		assert_eq!(
			files[0].config.tools["github:owner/empty"],
			ToolOptions::Github(GithubOptionsTable {
				version: "0.26.0".to_owned(),
				api_url: None,
				exposed: Some(Vec::new()),
			})
		);
		assert_eq!(
			files[0].config.tools["github:owner/exposed"],
			ToolOptions::Github(GithubOptionsTable {
				version: "0.26.0".to_owned(),
				api_url: None,
				exposed: Some(vec!["tool".to_owned(), "bin/*".to_owned()]),
			})
		);
	}

	#[test]
	fn keeps_files_separate() {
		let tmp = tempfile::tempdir().unwrap();
		let project_inner = _config(tmp.path(), "inner.toml", concat!(r#""github:a/one" = "4""#, "\n", r#""github:a/three" = "3""#));
		let project_outer = _config(tmp.path(), "outer.toml", r#""github:a/one" = "3""#);
		let system_env = _config(tmp.path(), "outpost.dev.toml", r#""github:a/one" = "2""#);
		let system = _config(tmp.path(), "config.toml", concat!(r#""github:a/one" = "1""#, "\n", r#""github:a/two" = "1""#));
		let files = load(&ConfigPaths {
			system: vec![system_env, system],
			project: vec![project_inner, project_outer],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 4);
		assert_eq!(files[0].config.tools.len(), 2);
		assert_eq!(_version(&files[0].config.tools["github:a/one"]), "4");
		assert_eq!(_version(&files[0].config.tools["github:a/three"]), "3");
		assert_eq!(files[1].config.tools.len(), 1);
		assert_eq!(_version(&files[1].config.tools["github:a/one"]), "3");
		assert_eq!(files[2].config.tools.len(), 1);
		assert_eq!(_version(&files[2].config.tools["github:a/one"]), "2");
		assert_eq!(files[3].config.tools.len(), 2);
		assert_eq!(_version(&files[3].config.tools["github:a/one"]), "1");
		assert_eq!(_version(&files[3].config.tools["github:a/two"]), "1");
	}

	#[test]
	fn skips_missing_files() {
		let tmp = tempfile::tempdir().unwrap();
		let missing = tmp.path().join("missing.toml");
		let path = _config(tmp.path(), "outpost.toml", r#""github:owner/repo" = "latest""#);
		let files = load(&ConfigPaths {
			system: vec![missing],
			project: vec![path.clone()],
			..ConfigPaths::default()
		})
		.unwrap();
		assert_eq!(files.len(), 1);
		assert_eq!(files[0].path, path);
		assert_eq!(files[0].config.tools.len(), 1);
		assert_eq!(_version(&files[0].config.tools["github:owner/repo"]), "latest");
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

	#[test]
	fn rejects_invalid_tool_options() {
		let cases = [
			(r#""github:owner/repo" = { version = "1", token = "x" }"#, "unknown field"),
			(r#""gitlab:owner/repo" = { version = "1" }"#, "unsupported backend `gitlab`"),
			(r#""gitlab:owner/repo" = "1""#, "unsupported backend `gitlab`"),
			(r#""owner/repo" = "1""#, "missing backend in `owner/repo`"),
			(r#""github:owner/repo" = { version = "1", exposed = ["tool", ""] }"#, "invalid `exposed`"),
		];
		for (tools, needle) in cases {
			let tmp = tempfile::tempdir().unwrap();
			let path = _config(tmp.path(), "outpost.toml", tools);
			let error = load(&ConfigPaths {
				system: vec![path],
				project: vec![],
				..ConfigPaths::default()
			})
			.err()
			.unwrap();
			assert!(error.to_string().contains(needle), "{tools}: {error}");
		}
	}
}
