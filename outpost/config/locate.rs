use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub(crate) fn locate(cwd: &Path) -> Result<ConfigPaths, LocateError> {
	locate_with(cwd, &ConfigEnv::from_process())
}

fn locate_with(cwd: &Path, env: &ConfigEnv) -> Result<ConfigPaths, LocateError> {
	let cwd = std::path::absolute(cwd).map_err(|source| (cwd.to_path_buf(), source))?;
	let (system_dir, system) = system_configs(env.home_config_dir.as_deref(), env.config_dir.as_deref(), env.envs.as_deref())?;
	let system_dir = system_dir.map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir));
	let project = project_configs(&cwd, system_dir.as_deref());
	let registered = match system_dir.as_deref() {
		Some(config_dir) if config_dir.is_dir() => registered_configs(config_dir, &project)?,
		_ => Vec::new(),
	};
	Ok(ConfigPaths {
		system_dir,
		system,
		project,
		registered,
	})
}

fn system_configs(home_config_dir: Option<&Path>, config_dir: Option<&Path>, envs: Option<&str>) -> Result<(Option<PathBuf>, Vec<PathBuf>), LocateError> {
	let mut files = Vec::new();
	let dir = if let Some(path) = config_dir.filter(|path| !path.is_empty()) {
		if !path.is_absolute() {
			return Err(LocateError::RelativeConfigDir(path.to_path_buf()));
		}
		files.push(path.join("outpost.toml"));
		path
	} else if let Some(path) = home_config_dir {
		files.push(path.join("config.toml"));
		path
	} else {
		return Ok((None, files));
	};
	if let Some(envs) = envs {
		let mut seen = HashSet::new();
		for env in envs.split(',') {
			let env = env.trim();
			if !env.is_empty() && seen.insert(env) {
				files.push(dir.join(format!("outpost.{env}.toml")));
			}
		}
	}
	Ok((Some(dir.to_path_buf()), files))
}

fn project_configs(cwd: &Path, config_dir: Option<&Path>) -> Vec<PathBuf> {
	let mut configs: Vec<PathBuf> = cwd
		.ancestors()
		.map(|dir| dir.join("outpost.toml"))
		.filter(|path| path.is_file())
		// skip the system config dir, including through symlinks; hardlinks are not detected
		.map(|path| std::fs::canonicalize(&path).unwrap_or(path))
		.filter(|path| config_dir.is_none_or(|config_dir| path.parent() != Some(config_dir)))
		.collect();
	configs.reverse();
	configs
}

fn registered_configs(config_dir: &Path, project_configs: &[PathBuf]) -> Result<Vec<PathBuf>, LocateError> {
	let registry = config_dir.join("configs.toml");
	let (text, registered) = match std::fs::read_to_string(&registry) {
		Ok(text) if text.trim().is_empty() => (Some(text), Vec::new()),
		Ok(text) => {
			let data = toml::from_str::<Registry>(&text).map_err(|source| (registry.clone(), source))?;
			(Some(text), data.paths)
		}
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, Vec::new()),
		Err(source) => return Err((registry.clone(), source).into()),
	};
	// registry paths are written canonical
	let mut paths: Vec<_> = registered
		.into_iter()
		.chain(project_configs.iter().cloned())
		.filter(|path| path.parent() != Some(config_dir))
		.collect();
	paths.sort();
	paths.dedup();
	_write_registry(&registry, text.as_deref(), &paths)?;
	let project: HashSet<_> = project_configs.iter().collect();
	paths.retain(|path| !project.contains(path));
	Ok(paths)
}

fn _write_registry(path: &Path, text: Option<&str>, paths: &[PathBuf]) -> Result<(), LocateError> {
	let serialized = toml::to_string_pretty(&RegistryRef { paths })?;
	if text == Some(serialized.as_str()) {
		return Ok(());
	}
	let temp = path.with_extension("toml.tmp");
	std::fs::write(&temp, serialized).map_err(|source| (temp.clone(), source))?;
	std::fs::rename(&temp, path).map_err(|source| (path.to_path_buf(), source))?;
	Ok(())
}

#[derive(Default)]
struct ConfigEnv {
	home_config_dir: Option<PathBuf>,
	config_dir: Option<PathBuf>,
	envs: Option<String>,
}

impl ConfigEnv {
	fn from_process() -> Self {
		Self {
			home_config_dir: std::env::home_dir().map(|home| home.join(".outpost")),
			config_dir: std::env::var_os("OUTPOST_CONFIG_DIR").map(PathBuf::from),
			envs: std::env::var("OUTPOST_ENV").ok(),
		}
	}
}

#[derive(Default)]
pub(crate) struct ConfigPaths {
	pub(crate) system_dir: Option<PathBuf>,
	pub(crate) system: Vec<PathBuf>,
	pub(crate) project: Vec<PathBuf>,
	pub(crate) registered: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Registry {
	#[serde(default)]
	paths: Vec<PathBuf>,
}

#[derive(Serialize)]
struct RegistryRef<'a> {
	paths: &'a [PathBuf],
}

#[derive(Debug)]
pub(crate) enum LocateError {
	Io { path: PathBuf, source: std::io::Error },
	RelativeConfigDir(PathBuf),
	RegistryParse { path: PathBuf, source: toml::de::Error },
	RegistrySerialize(toml::ser::Error),
}

impl From<(PathBuf, std::io::Error)> for LocateError {
	fn from((path, source): (PathBuf, std::io::Error)) -> Self {
		Self::Io { path, source }
	}
}

impl From<(PathBuf, toml::de::Error)> for LocateError {
	fn from((path, source): (PathBuf, toml::de::Error)) -> Self {
		Self::RegistryParse { path, source }
	}
}

impl From<toml::ser::Error> for LocateError {
	fn from(source: toml::ser::Error) -> Self {
		Self::RegistrySerialize(source)
	}
}

impl fmt::Display for LocateError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => write!(f, "failed to access `{}`: {source}", path.display()),
			Self::RelativeConfigDir(path) => {
				write!(f, "OUTPOST_CONFIG_DIR must be absolute, got `{}`", path.display())
			}
			Self::RegistryParse { path, source } => write!(f, "failed to parse config registry `{}`: {source}", path.display()),
			Self::RegistrySerialize(error) => write!(f, "failed to serialize config registry: {error}"),
		}
	}
}

impl std::error::Error for LocateError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::RelativeConfigDir(_) => None,
			Self::RegistryParse { source, .. } => Some(source),
			Self::RegistrySerialize(error) => Some(error),
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	pub(super) fn touch(path: &Path) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, "").unwrap();
	}

	#[test]
	fn locates_system_and_project_configs_without_env() {
		let tmp = tempfile::tempdir().unwrap();

		// flat project
		let flat = tmp.path().join("flat");
		touch(&flat.join("outpost.toml"));
		let home_config_dir = tmp.path().join("home").join(".outpost");
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&flat, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
		assert_eq!(locations.system, vec![home_config_dir.join("config.toml")]);
		assert_eq!(locations.project, vec![std::fs::canonicalize(flat.join("outpost.toml")).unwrap()]);

		// nested project
		let nested = tmp.path().join("nested");
		let cwd = nested.join("src");
		touch(&nested.join("outpost.toml"));
		touch(&cwd.join("outpost.toml"));
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
		assert_eq!(locations.system, vec![home_config_dir.join("config.toml")]);
		assert_eq!(locations.project, vec![
			std::fs::canonicalize(nested.join("outpost.toml")).unwrap(),
			std::fs::canonicalize(cwd.join("outpost.toml")).unwrap()
		]);
	}

	#[test]
	fn locates_system_configs_with_env() {
		let tmp = tempfile::tempdir().unwrap();
		let cwd = tmp.path().join("cwd");

		// home + envs
		let home_config_dir = tmp.path().join("home").join(".outpost");
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir.clone()),
			envs: Some("dev,prod".to_owned()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
		assert_eq!(locations.system, vec![
			home_config_dir.join("config.toml"),
			home_config_dir.join("outpost.dev.toml"),
			home_config_dir.join("outpost.prod.toml"),
		]);

		// config
		let config_dir = tmp.path().join("config");
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir.clone()),
			config_dir: Some(config_dir.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.toml")]);

		// config + empty
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir.clone()),
			config_dir: Some(PathBuf::new()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
		assert_eq!(locations.system, vec![home_config_dir.join("config.toml")]);

		// config + relative
		let env = ConfigEnv {
			config_dir: Some(PathBuf::from("relative")),
			..ConfigEnv::default()
		};
		assert!(matches!(
			locate_with(&cwd, &env),
			Err(LocateError::RelativeConfigDir(path)) if path == Path::new("relative")
		));

		// config + envs
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir),
			config_dir: Some(config_dir.clone()),
			envs: Some("dev".to_owned()),
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.toml"), config_dir.join("outpost.dev.toml"),]);

		// config + duplicate envs
		let env = ConfigEnv {
			config_dir: Some(config_dir.clone()),
			envs: Some("dev,dev".to_owned()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.toml"), config_dir.join("outpost.dev.toml"),]);
	}

	#[test]
	fn excludes_config_dir_from_project_configs() {
		let tmp = tempfile::tempdir().unwrap();

		// home
		let home_config_dir = tmp.path().join("home").join(".outpost");
		touch(&home_config_dir.join("config.toml"));
		touch(&home_config_dir.join("outpost.toml"));
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&home_config_dir, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&std::fs::canonicalize(&home_config_dir).unwrap()));
		assert_eq!(locations.system, vec![home_config_dir.join("config.toml")]);
		assert!(locations.project.is_empty());

		// config
		let nested = tmp.path().join("nested");
		let cwd = nested.join("config");
		touch(&nested.join("outpost.toml"));
		touch(&cwd.join("outpost.toml"));
		let env = ConfigEnv {
			config_dir: Some(cwd.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&std::fs::canonicalize(&cwd).unwrap()));
		assert_eq!(locations.system, vec![cwd.join("outpost.toml")]);
		assert_eq!(locations.project, vec![std::fs::canonicalize(nested.join("outpost.toml")).unwrap()]);
	}

	#[test]
	fn excludes_config_dir_through_symlink() {
		let tmp = tempfile::tempdir().unwrap();
		let config_dir = tmp.path().join("config");
		fs::create_dir_all(&config_dir).unwrap();
		touch(&config_dir.join("outpost.toml"));
		let alias = tmp.path().join("alias");

		#[cfg(unix)]
		let linked = std::os::unix::fs::symlink(&config_dir, &alias).is_ok();
		#[cfg(windows)]
		let linked = std::os::windows::fs::symlink_dir(&config_dir, &alias).is_ok();
		#[cfg(not(any(unix, windows)))]
		let linked = false;
		if !linked {
			eprintln!("skipping: cannot create a directory symlink");
			return;
		}

		let env = ConfigEnv {
			config_dir: Some(config_dir.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&alias, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&std::fs::canonicalize(&config_dir).unwrap()));
		assert_eq!(locations.system, vec![config_dir.join("outpost.toml")]);
		assert!(locations.project.is_empty());
	}
}

#[cfg(test)]
mod registry_tests {
	use std::fs;

	use super::tests::touch;
	use super::*;

	#[test]
	fn registers_project_configs_for_other_directories() {
		let tmp = tempfile::tempdir().unwrap();
		let config_dir = tmp.path().join("config");
		fs::create_dir_all(&config_dir).unwrap();
		let project = tmp.path().join("project");
		let config = project.join("outpost.toml");
		touch(&config);
		let env = ConfigEnv {
			config_dir: Some(config_dir.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&project, &env).unwrap();
		assert_eq!(locations.project, vec![std::fs::canonicalize(&config).unwrap()]);
		assert!(locations.registered.is_empty());

		let other = tmp.path().join("other");
		fs::create_dir_all(&other).unwrap();
		let locations = locate_with(&other, &env).unwrap();
		assert!(locations.project.is_empty());
		assert_eq!(locations.registered, vec![std::fs::canonicalize(&config).unwrap()]);

		let registry = fs::read_to_string(config_dir.join("configs.toml")).unwrap();
		let registry: Registry = toml::from_str(&registry).unwrap();
		assert_eq!(registry.paths, vec![std::fs::canonicalize(&config).unwrap()]);
	}

	#[test]
	fn dedupes_registered_project_configs() {
		let tmp = tempfile::tempdir().unwrap();
		let config_dir = tmp.path().join("config");
		fs::create_dir_all(&config_dir).unwrap();
		let project = tmp.path().join("project");
		let config = project.join("outpost.toml");
		touch(&config);
		let env = ConfigEnv {
			config_dir: Some(config_dir.clone()),
			..ConfigEnv::default()
		};
		let config = std::fs::canonicalize(&config).unwrap();
		let locations = locate_with(&project, &env).unwrap();
		assert_eq!(locations.project, vec![config.clone()]);
		assert!(locations.registered.is_empty());

		let locations = locate_with(&project, &env).unwrap();
		assert_eq!(locations.project, vec![config.clone()]);
		assert!(locations.registered.is_empty());

		let registry = fs::read_to_string(config_dir.join("configs.toml")).unwrap();
		let registry: Registry = toml::from_str(&registry).unwrap();
		assert_eq!(registry.paths, vec![config]);
	}

	#[test]
	fn skips_registry_without_system_dir() {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		touch(&project.join("outpost.toml"));
		let locations = locate_with(&project, &ConfigEnv::default()).unwrap();
		assert!(locations.system_dir.is_none());
		assert!(locations.registered.is_empty());
	}

	#[test]
	fn rejects_invalid_registry() {
		let tmp = tempfile::tempdir().unwrap();
		let config_dir = tmp.path().join("config");
		fs::create_dir_all(&config_dir).unwrap();
		fs::write(config_dir.join("configs.toml"), "[paths\n").unwrap();
		let env = ConfigEnv {
			config_dir: Some(config_dir),
			..ConfigEnv::default()
		};
		let project = tmp.path().join("project");
		touch(&project.join("outpost.toml"));
		assert!(matches!(locate_with(&project, &env), Err(LocateError::RegistryParse { .. })));
	}
}
