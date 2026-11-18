use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

use super::state::{self, State, StateError};

pub(crate) fn locate(cwd: &Path) -> Result<ConfigPaths, LocateError> {
	locate_with(cwd, &ConfigEnv::from_process())
}

fn locate_with(cwd: &Path, env: &ConfigEnv) -> Result<ConfigPaths, LocateError> {
	let cwd = std::path::absolute(cwd).map_err(|source| (cwd.to_path_buf(), source))?;
	let (system_dir, system) = system_configs(env.home_config_dir.as_deref(), env.config_dir.as_deref(), env.envs.as_deref())?;
	let system_dir = system_dir.map(|dir| std::fs::canonicalize(&dir).unwrap_or(dir));
	let downloads_dir = match env.downloads_dir.as_deref().filter(|path| !path.as_os_str().is_empty()) {
		Some(path) if !path.is_absolute() => {
			return Err(LocateError::RelativeDir {
				name: "OUTPOST_DOWNLOADS_DIR",
				path: path.to_path_buf(),
			});
		}
		Some(path) => Some(path.to_path_buf()),
		None => system_dir.as_ref().map(|dir| dir.join("downloads")),
	};
	let tools_dir = match env.tools_dir.as_deref().filter(|path| !path.as_os_str().is_empty()) {
		Some(path) if !path.is_absolute() => {
			return Err(LocateError::RelativeDir {
				name: "OUTPOST_TOOLS_DIR",
				path: path.to_path_buf(),
			});
		}
		Some(path) => Some(path.to_path_buf()),
		None => system_dir.as_ref().map(|dir| dir.join("tools")),
	};
	let shims_dir = match env.shims_dir.as_deref().filter(|path| !path.as_os_str().is_empty()) {
		Some(path) if !path.is_absolute() => {
			return Err(LocateError::RelativeDir {
				name: "OUTPOST_SHIMS_DIR",
				path: path.to_path_buf(),
			});
		}
		Some(path) => Some(path.to_path_buf()),
		None => system_dir.as_ref().map(|dir| dir.join("shims")),
	};
	let project = project_configs(&cwd, system_dir.as_deref());
	let state = match system_dir.as_deref().filter(|dir| dir.is_dir()) {
		Some(config_dir) => {
			let (mut state, _) = state::read(config_dir)?;
			let project: HashSet<_> = project.iter().collect();
			state.registered_paths.retain(|path| !project.contains(path));
			state
		}
		None => State::default(),
	};
	Ok(ConfigPaths {
		system_dir,
		downloads_dir,
		tools_dir,
		shims_dir,
		system,
		project,
		state,
	})
}

fn system_configs(home_config_dir: Option<&Path>, config_dir: Option<&Path>, envs: Option<&str>) -> Result<(Option<PathBuf>, Vec<PathBuf>), LocateError> {
	let mut files = Vec::new();
	let (dir, file) = if let Some(path) = config_dir.filter(|path| !path.is_empty()) {
		if !path.is_absolute() {
			return Err(LocateError::RelativeDir {
				name: "OUTPOST_CONFIG_DIR",
				path: path.to_path_buf(),
			});
		}
		(path, "outpost.toml")
	} else if let Some(path) = home_config_dir {
		(path, "config.toml")
	} else {
		return Ok((None, files));
	};
	// env files come first so they take precedence over the base file
	if let Some(envs) = envs {
		let mut seen = HashSet::new();
		for env in envs.split(',') {
			let env = env.trim();
			if !env.is_empty() && seen.insert(env) {
				files.push(dir.join(format!("outpost.{env}.toml")));
			}
		}
	}
	files.push(dir.join(file));
	Ok((Some(dir.to_path_buf()), files))
}

fn project_configs(cwd: &Path, config_dir: Option<&Path>) -> Vec<PathBuf> {
	let configs: Vec<PathBuf> = cwd
		.ancestors()
		.map(|dir| dir.join("outpost.toml"))
		.filter(|path| path.is_file())
		// skip the system config dir, including through symlinks; hardlinks are not detected
		.map(|path| std::fs::canonicalize(&path).unwrap_or(path))
		.filter(|path| config_dir.is_none_or(|config_dir| path.parent() != Some(config_dir)))
		.collect();
	configs
}

#[derive(Default)]
struct ConfigEnv {
	home_config_dir: Option<PathBuf>,
	config_dir: Option<PathBuf>,
	downloads_dir: Option<PathBuf>,
	tools_dir: Option<PathBuf>,
	shims_dir: Option<PathBuf>,
	envs: Option<String>,
}

impl ConfigEnv {
	fn from_process() -> Self {
		Self {
			home_config_dir: std::env::home_dir().map(|home| home.join(".outpost")),
			config_dir: std::env::var_os("OUTPOST_CONFIG_DIR").map(PathBuf::from),
			downloads_dir: std::env::var_os("OUTPOST_DOWNLOADS_DIR").map(PathBuf::from),
			tools_dir: std::env::var_os("OUTPOST_TOOLS_DIR").map(PathBuf::from),
			shims_dir: std::env::var_os("OUTPOST_SHIMS_DIR").map(PathBuf::from),
			envs: std::env::var("OUTPOST_ENV").ok(),
		}
	}
}

#[derive(Default)]
pub(crate) struct ConfigPaths {
	pub(crate) system_dir: Option<PathBuf>,
	pub(crate) downloads_dir: Option<PathBuf>,
	pub(crate) tools_dir: Option<PathBuf>,
	pub(crate) shims_dir: Option<PathBuf>,
	pub(crate) system: Vec<PathBuf>,
	pub(crate) project: Vec<PathBuf>,
	pub(crate) state: State,
}

#[derive(Debug)]
pub(crate) enum LocateError {
	Io { path: PathBuf, source: std::io::Error },
	RelativeDir { name: &'static str, path: PathBuf },
	State(StateError),
}

impl From<(PathBuf, std::io::Error)> for LocateError {
	fn from((path, source): (PathBuf, std::io::Error)) -> Self {
		Self::Io { path, source }
	}
}

impl From<StateError> for LocateError {
	fn from(error: StateError) -> Self {
		Self::State(error)
	}
}

impl fmt::Display for LocateError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io { path, source } => write!(f, "failed to access `{}`: {source}", path.display()),
			Self::RelativeDir { name, path } => write!(f, "{name} must be absolute, got `{}`", path.display()),
			Self::State(error) => fmt::Display::fmt(error, f),
		}
	}
}

impl std::error::Error for LocateError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io { source, .. } => Some(source),
			Self::RelativeDir { .. } => None,
			Self::State(error) => Some(error),
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
		assert_eq!(locations.downloads_dir, Some(home_config_dir.join("downloads")));
		assert_eq!(locations.tools_dir, Some(home_config_dir.join("tools")));
		assert_eq!(locations.shims_dir, Some(home_config_dir.join("shims")));
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
			std::fs::canonicalize(cwd.join("outpost.toml")).unwrap(),
			std::fs::canonicalize(nested.join("outpost.toml")).unwrap()
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
			home_config_dir.join("outpost.dev.toml"),
			home_config_dir.join("outpost.prod.toml"),
			home_config_dir.join("config.toml"),
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
			Err(LocateError::RelativeDir { name: "OUTPOST_CONFIG_DIR", path }) if path == Path::new("relative")
		));

		// config + envs
		let env = ConfigEnv {
			home_config_dir: Some(home_config_dir),
			config_dir: Some(config_dir.clone()),
			envs: Some("dev".to_owned()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.dev.toml"), config_dir.join("outpost.toml"),]);

		// config + duplicate envs
		let env = ConfigEnv {
			config_dir: Some(config_dir.clone()),
			envs: Some("dev,dev".to_owned()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.dev.toml"), config_dir.join("outpost.toml"),]);
	}

	#[test]
	fn locates_custom_downloads_with_env() {
		let tmp = tempfile::tempdir().unwrap();
		let cwd = tmp.path().join("cwd");
		let home = tmp.path().join("home").join(".outpost");
		let downloads = tmp.path().join("downloads");
		// explicit directory
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			downloads_dir: Some(downloads.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home));
		assert_eq!(locations.downloads_dir, Some(downloads));
		// empty value falls back to the default
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			downloads_dir: Some(PathBuf::new()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.downloads_dir, Some(home.join("downloads")));
		// relative value is rejected
		let env = ConfigEnv {
			downloads_dir: Some(PathBuf::from("relative")),
			..ConfigEnv::default()
		};
		assert!(matches!(
			locate_with(&cwd, &env),
			Err(LocateError::RelativeDir { name: "OUTPOST_DOWNLOADS_DIR", path }) if path == Path::new("relative")
		));
	}

	#[test]
	fn locates_custom_tools_with_env() {
		let tmp = tempfile::tempdir().unwrap();
		let cwd = tmp.path().join("cwd");
		let home = tmp.path().join("home").join(".outpost");
		let tools = tmp.path().join("tools");
		// explicit directory
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			tools_dir: Some(tools.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home));
		assert_eq!(locations.tools_dir, Some(tools));
		// empty value falls back to the default
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			tools_dir: Some(PathBuf::new()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.tools_dir, Some(home.join("tools")));
		// relative value is rejected
		let env = ConfigEnv {
			tools_dir: Some(PathBuf::from("relative")),
			..ConfigEnv::default()
		};
		assert!(matches!(
			locate_with(&cwd, &env),
			Err(LocateError::RelativeDir { name: "OUTPOST_TOOLS_DIR", path }) if path == Path::new("relative")
		));
	}

	#[test]
	fn locates_custom_shims_with_env() {
		let tmp = tempfile::tempdir().unwrap();
		let cwd = tmp.path().join("cwd");
		let home = tmp.path().join("home").join(".outpost");
		let shims = tmp.path().join("shims");
		// explicit directory
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			shims_dir: Some(shims.clone()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home));
		assert_eq!(locations.shims_dir, Some(shims));
		// empty value falls back to the default
		let env = ConfigEnv {
			home_config_dir: Some(home.clone()),
			shims_dir: Some(PathBuf::new()),
			..ConfigEnv::default()
		};
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.shims_dir, Some(home.join("shims")));
		// relative value is rejected
		let env = ConfigEnv {
			shims_dir: Some(PathBuf::from("relative")),
			..ConfigEnv::default()
		};
		assert!(matches!(
			locate_with(&cwd, &env),
			Err(LocateError::RelativeDir { name: "OUTPOST_SHIMS_DIR", path }) if path == Path::new("relative")
		));
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
mod state_tests {
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
		let config = std::fs::canonicalize(&config).unwrap();
		let locations = locate_with(&project, &env).unwrap();
		let mut registered = State::default();
		state::register_config(&mut registered, &config_dir, std::slice::from_ref(&config));
		state::write(&config_dir, &registered, None).unwrap();
		assert_eq!(locations.project, vec![config.clone()]);
		assert!(locations.state.registered_paths.is_empty());

		let other = tmp.path().join("other");
		fs::create_dir_all(&other).unwrap();
		let locations = locate_with(&other, &env).unwrap();
		assert!(locations.project.is_empty());
		assert_eq!(locations.state.registered_paths, vec![config]);
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
		let mut registered = State::default();
		state::register_config(&mut registered, &config_dir, std::slice::from_ref(&config));
		state::write(&config_dir, &registered, None).unwrap();
		assert_eq!(locations.project, vec![config.clone()]);
		assert!(locations.state.registered_paths.is_empty());

		let locations = locate_with(&project, &env).unwrap();
		state::register_config(&mut registered, &config_dir, std::slice::from_ref(&config));
		state::write(&config_dir, &registered, None).unwrap();
		assert_eq!(locations.project, vec![config.clone()]);
		assert!(locations.state.registered_paths.is_empty());

		let text = fs::read_to_string(config_dir.join("state.toml")).unwrap();
		let state: State = toml::from_str(&text).unwrap();
		assert_eq!(state.registered_paths, vec![config]);
	}

	#[test]
	fn skips_state_without_system_dir() {
		let tmp = tempfile::tempdir().unwrap();
		let project = tmp.path().join("project");
		touch(&project.join("outpost.toml"));
		let locations = locate_with(&project, &ConfigEnv::default()).unwrap();
		assert!(locations.system_dir.is_none());
		assert!(locations.state.registered_paths.is_empty());
	}

	#[test]
	fn rejects_invalid_state() {
		let tmp = tempfile::tempdir().unwrap();
		let config_dir = tmp.path().join("config");
		fs::create_dir_all(&config_dir).unwrap();
		fs::write(config_dir.join("state.toml"), "[registered_paths\n").unwrap();
		let env = ConfigEnv {
			config_dir: Some(config_dir),
			..ConfigEnv::default()
		};
		let project = tmp.path().join("project");
		touch(&project.join("outpost.toml"));
		assert!(matches!(locate_with(&project, &env), Err(LocateError::State(StateError::Parse { .. }))));
	}
}
