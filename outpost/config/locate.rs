use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};

pub(crate) fn locate(cwd: &Path) -> Result<ConfigPaths, LocateError> {
	locate_with(cwd, &ConfigEnv::from_process())
}

fn locate_with(cwd: &Path, env: &ConfigEnv) -> Result<ConfigPaths, LocateError> {
	let cwd = std::path::absolute(cwd)?;
	let home_config_dir = env.home_config_dir.as_deref().map(std::path::absolute).transpose()?;
	let config_dir = match env.config_dir.as_deref() {
		Some(dir) if dir.is_empty() => None,
		Some(dir) if dir.is_absolute() => Some(std::path::absolute(dir)?),
		Some(dir) => return Err(LocateError::RelativeConfigDir(dir.to_path_buf())),
		None => None,
	};
	let system_dir = config_dir.clone().or_else(|| home_config_dir.clone());
	let system = system_configs(home_config_dir.as_deref(), config_dir.as_deref(), env.envs.as_deref());
	let project = project_configs(&cwd, system_dir.as_deref());
	Ok(ConfigPaths { system_dir, system, project })
}

fn system_configs(home_config_dir: Option<&Path>, config_dir: Option<&Path>, envs: Option<&str>) -> Vec<PathBuf> {
	let mut files = Vec::new();
	let dir = if let Some(path) = config_dir {
		files.push(path.join("outpost.toml"));
		path
	} else if let Some(path) = home_config_dir {
		files.push(path.join("config.toml"));
		path
	} else {
		return files;
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
	files
}

fn project_configs(cwd: &Path, config_dir: Option<&Path>) -> Vec<PathBuf> {
	let config_dir = config_dir.map(|dir| std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
	let mut configs: Vec<PathBuf> = cwd
		.ancestors()
		.map(|dir| dir.join("outpost.toml"))
		.filter(|path| path.is_file())
		// skip the system config dir, including through symlinks; hardlinks are not detected
		.filter(|path| {
			config_dir
				.as_deref()
				.is_none_or(|config_dir| path.parent() != Some(config_dir) && std::fs::canonicalize(path).map_or(true, |path| path.parent() != Some(config_dir)))
		})
		.collect();
	configs.reverse();
	configs
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
}

#[derive(Debug)]
pub(crate) enum LocateError {
	Io(std::io::Error),
	RelativeConfigDir(PathBuf),
}

impl From<std::io::Error> for LocateError {
	fn from(error: std::io::Error) -> Self {
		Self::Io(error)
	}
}

impl fmt::Display for LocateError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Io(error) => write!(f, "{error}"),
			Self::RelativeConfigDir(path) => {
				write!(f, "OUTPOST_CONFIG_DIR must be absolute, got `{}`", path.display())
			}
		}
	}
}

impl std::error::Error for LocateError {
	fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
		match self {
			Self::Io(error) => Some(error),
			Self::RelativeConfigDir(_) => None,
		}
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	fn touch(path: &Path) {
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
		assert_eq!(locations.project, vec![flat.join("outpost.toml")]);

		// nested project
		let nested = tmp.path().join("nested");
		let cwd = nested.join("src");
		touch(&nested.join("outpost.toml"));
		touch(&cwd.join("outpost.toml"));
		let locations = locate_with(&cwd, &env).unwrap();
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
		assert_eq!(locations.system, vec![home_config_dir.join("config.toml")]);
		assert_eq!(locations.project, vec![nested.join("outpost.toml"), cwd.join("outpost.toml")]);
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
		assert_eq!(locations.system_dir.as_ref(), Some(&home_config_dir));
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
		assert_eq!(locations.system_dir.as_ref(), Some(&cwd));
		assert_eq!(locations.system, vec![cwd.join("outpost.toml")]);
		assert_eq!(locations.project, vec![nested.join("outpost.toml")]);
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
		assert_eq!(locations.system_dir.as_ref(), Some(&config_dir));
		assert_eq!(locations.system, vec![config_dir.join("outpost.toml")]);
		assert!(locations.project.is_empty());
	}
}
