use std::fmt;

use regex::regex;
use semver::Version as Semver;

#[derive(Debug, PartialEq, Eq)]
pub enum Version {
	Latest,
	Prefix(Prefix),
	Exact(String),
}

impl Version {
	pub fn parse(input: &str) -> Self {
		let input = input.trim();
		if input == "latest" {
			Self::Latest
		} else if let Some(prefix) = Prefix::parse(input) {
			Self::Prefix(prefix)
		} else {
			Self::Exact(input.to_owned())
		}
	}
}

#[derive(Debug, PartialEq, Eq)]
pub struct Prefix {
	parts: Vec<u64>,
}

impl Prefix {
	fn parse(input: &str) -> Option<Self> {
		let parts = input.split('.').map(str::parse::<u64>).collect::<Result<Vec<_>, _>>().ok()?;
		(1..=2).contains(&parts.len()).then_some(Self { parts })
	}

	pub fn matches(&self, tag: &str) -> Option<Semver> {
		let version = tag_version(tag)?;
		let components = [version.major, version.minor, version.patch];
		(self.parts[..] == components[..self.parts.len()]).then_some(version)
	}
}

impl fmt::Display for Prefix {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		for (index, part) in self.parts.iter().enumerate() {
			if index > 0 {
				write!(f, ".")?;
			}
			write!(f, "{part}")?;
		}
		Ok(())
	}
}

pub fn split_lane(input: &str) -> (&str, &str) {
	match regex!(r"^(?:(?<lane>.*?[_@/-]))?(?<version>v?[0-9].*|latest)$").captures(input) {
		Some(captures) => {
			let lane = captures.name("lane").map_or(0..0, |lane| lane.range());
			let version = captures.name("version").unwrap().range();
			(&input[lane], &input[version])
		}
		None => ("", input),
	}
}

pub fn tag_version(tag: &str) -> Option<Semver> {
	let tag = tag.strip_prefix('v').unwrap_or(tag);
	if let Ok(version) = Semver::parse(tag) {
		return Some(version);
	}
	let parts: Vec<u64> = tag.split('.').map(str::parse).collect::<Result<_, _>>().ok()?;
	if parts.len() > 3 {
		return None;
	}
	Some(Semver::new(parts[0], parts.get(1).copied().unwrap_or(0), parts.get(2).copied().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_latest() {
		assert_eq!(Version::parse("latest"), Version::Latest);
	}

	#[test]
	fn parses_prefixes() {
		assert_eq!(Version::parse("2"), Version::Prefix(Prefix { parts: vec![2] }));
		assert_eq!(Version::parse("2.20"), Version::Prefix(Prefix { parts: vec![2, 20] }));
	}

	#[test]
	fn displays_prefix() {
		assert_eq!(Prefix { parts: vec![2, 20] }.to_string(), "2.20");
	}

	#[test]
	fn parses_exact_strings() {
		for value in ["2.20.1", "v2", "v2.20", "v2.20.1", "release-2.20.1", "2.20.1-rc.1", "2.x", "2.20.1.0"] {
			assert_eq!(Version::parse(value), Version::Exact(value.to_owned()), "{value}");
		}
	}

	#[test]
	fn matches_leading_components() {
		assert!(Prefix { parts: vec![2] }.matches("v2.20.1").is_some());
		assert!(Prefix { parts: vec![2] }.matches("2.0.0").is_some());
		assert!(Prefix { parts: vec![2] }.matches("3.0.0").is_none());

		assert!(Prefix { parts: vec![2, 20] }.matches("v2.20.1").is_some());
		assert!(Prefix { parts: vec![2, 20] }.matches("v2.200.1").is_none());
		assert!(Prefix { parts: vec![2, 20] }.matches("v2.21.0").is_none());
	}

	#[test]
	fn matches_prerelease_and_build_metadata() {
		let version = Prefix { parts: vec![2, 20] }.matches("v2.20.1-rc.1").unwrap();
		assert_eq!(version.pre.as_str(), "rc.1");
		let version = Prefix { parts: vec![2, 20] }.matches("v2.20.1+build.5").unwrap();
		assert_eq!(version.build.as_str(), "build.5");
	}

	#[test]
	fn matches_non_semver_numeric_tags() {
		assert_eq!(Prefix { parts: vec![2, 20] }.matches("v2.20").unwrap(), Semver::new(2, 20, 0));
		assert_eq!(Prefix { parts: vec![2] }.matches("v02").unwrap(), Semver::new(2, 0, 0));
		assert_eq!(Prefix { parts: vec![2] }.matches("02").unwrap(), Semver::new(2, 0, 0));
	}

	#[test]
	fn rejects_extra_components() {
		assert_eq!(Version::parse("2.20.1.0"), Version::Exact("2.20.1.0".to_owned()));
	}

	#[test]
	fn rejects_unparsable_tags() {
		for tag in ["nightly", "2.20.1.0", "v", ""] {
			assert!(Prefix { parts: vec![2] }.matches(tag).is_none(), "{tag}");
		}
	}
}
