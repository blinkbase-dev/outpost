use regex::regex;

use super::ArchiveFormat;

pub(crate) fn select<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<(usize, &'a str, ArchiveFormat)> {
	select_with(names, &Target::current())
}

fn select_with<'a>(names: impl IntoIterator<Item = &'a str>, target: &Target) -> Option<(usize, &'a str, ArchiveFormat)> {
	names
		.into_iter()
		.enumerate()
		.filter_map(|(index, name)| {
			let format = ArchiveFormat::from_name(name)?;
			let score = score(name, &format, target)?;
			Some((score, index, name, format))
		})
		.max_by(|(left_score, _, left, ..), (right_score, _, right, ..)| left_score.cmp(right_score).then_with(|| right.cmp(left)))
		.map(|(_, index, name, format)| (index, name, format))
}

fn score(name: &str, format: &ArchiveFormat, target: &Target) -> Option<i32> {
	let name = name.to_ascii_lowercase();
	if exclusion_matches(&name) {
		return None;
	}
	let mut score = if os_matches(&name, target.os) {
		32
	} else if os_libc_matches(&name, target.os) {
		16
	} else {
		return None;
	};
	if arch_matches(&name, target.arch)? {
		score += 8;
	}
	if libc_matches(&name, target.libc) {
		score += 4;
	}
	score += match format {
		ArchiveFormat::TarXz => 3,
		ArchiveFormat::TarGz => 2,
		ArchiveFormat::Zip => 1,
	};
	Some(score)
}

#[derive(Debug)]
struct Target {
	os: &'static str,
	arch: &'static str,
	libc: &'static str,
}

impl Target {
	fn current() -> Self {
		let libc = if cfg!(target_env = "gnu") {
			"gnu"
		} else if cfg!(target_env = "musl") {
			"musl"
		} else if cfg!(target_env = "msvc") {
			"msvc"
		} else {
			""
		};
		Self {
			os: std::env::consts::OS,
			arch: std::env::consts::ARCH,
			libc,
		}
	}
}

fn exclusion_matches(name: &str) -> bool {
	let patterns = ["checksums", ".sha256", ".sha512", ".sig", ".asc", ".pem", ".txt"];
	patterns.iter().any(|pattern| name.contains(pattern))
}

fn os_matches(name: &str, os: &str) -> bool {
	match os {
		"linux" => regex!(r"(?:\b|_)linux"),
		"macos" => regex!(r"(?:\b|_)(?:macos|darwin|osx)"),
		"windows" => regex!(r"(?:\b|_)(?:windows|win32|win64)"),
		_ => return false,
	}
	.is_match(name)
}

fn os_libc_matches(name: &str, os: &str) -> bool {
	if ["linux", "macos", "windows"].into_iter().any(|other| other != os && os_matches(name, other)) {
		return false;
	}
	match os {
		"linux" => regex!(r"(?:\b|_)(?:gnu|musl)"),
		"windows" => regex!(r"(?:\b|_)(?:msvc|gnu|mingw)"),
		_ => return false,
	}
	.is_match(name)
}

fn arch_matches(name: &str, arch: &str) -> Option<bool> {
	let arm = regex!(r"(?:\b|_)(?:armv6|armv7|armhf)");
	let aarch64 = regex!(r"(?:\b|_)(?:aarch64|arm64|armv8)");
	let x86 = regex!(r"(?:\b|_)(?:x86|i686|i386|386|win32|msvc32|mingw32)");
	let x86_64 = regex!(r"(?:\b|_)(?:x86_64|x86-64|amd64|x64|win64|msvc64|mingw64)");
	let found = if aarch64.is_match(name) {
		Some("aarch64")
	} else if x86_64.is_match(name) {
		Some("x86_64")
	} else if x86.is_match(name) {
		Some("x86")
	} else if arm.is_match(name) {
		Some("arm")
	} else {
		None
	};
	match found {
		Some(found) if found != arch => None,
		Some(_) => Some(true),
		None => Some(false),
	}
}

fn libc_matches(name: &str, libc: &str) -> bool {
	match libc {
		"gnu" => regex!(r"(?:\b|_)(?:gnu|mingw)"),
		"musl" => regex!(r"(?:\b|_)musl"),
		"msvc" => regex!(r"(?:\b|_)msvc"),
		_ => return false,
	}
	.is_match(name)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn selects_os_arch() {
		let targets = [
			Target {
				os: "linux",
				arch: "aarch64",
				libc: "musl",
			},
			Target {
				os: "linux",
				arch: "x86_64",
				libc: "gnu",
			},
			Target {
				os: "macos",
				arch: "aarch64",
				libc: "",
			},
			Target {
				os: "windows",
				arch: "x86",
				libc: "msvc",
			},
			Target {
				os: "windows",
				arch: "x86_64",
				libc: "msvc",
			},
		];
		let names = [
			"tool-linux-arm64.tar.gz",
			"tool-linux-amd64.tar.gz",
			"tool-darwin-arm64.tar.gz",
			"tool-windows-i686.zip",
			"tool-windows-x86_64.zip",
		];
		let expected = [
			(0, "tool-linux-arm64.tar.gz", ArchiveFormat::TarGz),
			(1, "tool-linux-amd64.tar.gz", ArchiveFormat::TarGz),
			(2, "tool-darwin-arm64.tar.gz", ArchiveFormat::TarGz),
			(3, "tool-windows-i686.zip", ArchiveFormat::Zip),
			(4, "tool-windows-x86_64.zip", ArchiveFormat::Zip),
		];
		for (target, (index, name, format)) in targets.into_iter().zip(expected) {
			assert_eq!(select_with(names, &target), Some((index, name, format)), "{target:?}");
		}
	}

	#[test]
	fn selects_os_arch_matching_libc() {
		let targets = [
			Target {
				os: "linux",
				arch: "aarch64",
				libc: "musl",
			},
			Target {
				os: "linux",
				arch: "x86_64",
				libc: "gnu",
			},
			Target {
				os: "macos",
				arch: "aarch64",
				libc: "",
			},
			Target {
				os: "windows",
				arch: "x86",
				libc: "msvc",
			},
			Target {
				os: "windows",
				arch: "x86_64",
				libc: "msvc",
			},
		];
		let names = [
			"tool-linux-arm64-gnu.tar.gz",
			"tool-linux-arm64-musl.tar.gz",
			"tool-linux-amd64-gnu.tar.gz",
			"tool-linux-amd64-musl.tar.gz",
			"tool-darwin-arm64.tar.gz",
			"tool-windows-i686-msvc.zip",
			"tool-windows-i686-gnu.zip",
			"tool-windows-x86_64-msvc.zip",
			"tool-windows-x86_64-gnu.zip",
		];
		let expected = [
			(1, "tool-linux-arm64-musl.tar.gz", ArchiveFormat::TarGz),
			(2, "tool-linux-amd64-gnu.tar.gz", ArchiveFormat::TarGz),
			(4, "tool-darwin-arm64.tar.gz", ArchiveFormat::TarGz),
			(5, "tool-windows-i686-msvc.zip", ArchiveFormat::Zip),
			(7, "tool-windows-x86_64-msvc.zip", ArchiveFormat::Zip),
		];
		for (target, (index, name, format)) in targets.into_iter().zip(expected) {
			assert_eq!(select_with(names, &target), Some((index, name, format)), "{target:?}");
		}
	}

	#[test]
	fn selects_libc_only() {
		let targets = [
			Target {
				os: "linux",
				arch: "aarch64",
				libc: "musl",
			},
			Target {
				os: "linux",
				arch: "x86_64",
				libc: "gnu",
			},
			Target {
				os: "windows",
				arch: "x86",
				libc: "msvc",
			},
			Target {
				os: "windows",
				arch: "x86_64",
				libc: "msvc",
			},
		];
		let names = [
			"tool-musl.tar.gz", //
			"tool-gnu.tar.gz",
			"tool-msvc.zip",
			"tool-msvc32.zip",
			"tool-msvc64.zip",
		];
		let expected = [
			(0, "tool-musl.tar.gz", ArchiveFormat::TarGz), //
			(1, "tool-gnu.tar.gz", ArchiveFormat::TarGz),
			(3, "tool-msvc32.zip", ArchiveFormat::Zip),
			(4, "tool-msvc64.zip", ArchiveFormat::Zip),
		];
		for (target, (index, name, format)) in targets.into_iter().zip(expected) {
			assert_eq!(select_with(names, &target), Some((index, name, format)), "{target:?}");
		}
	}

	#[test]
	fn selects_libc_windows_unnamed() {
		let targets = [
			Target {
				os: "linux",
				arch: "aarch64",
				libc: "musl",
			},
			Target {
				os: "linux",
				arch: "x86_64",
				libc: "gnu",
			},
			Target {
				os: "macos",
				arch: "aarch64",
				libc: "",
			},
			Target {
				os: "windows",
				arch: "x86",
				libc: "msvc",
			},
			Target {
				os: "windows",
				arch: "x86_64",
				libc: "msvc",
			},
		];
		let names = [
			"tool-linux-arm64.tar.gz",
			"tool-linux-amd64.tar.gz",
			"tool-darwin-arm64.tar.gz",
			"tool-mingw32.zip",
			"tool-mingw64.zip",
			"tool-msvc32.zip",
			"tool-msvc64.zip",
		];
		let expected = [
			(0, "tool-linux-arm64.tar.gz", ArchiveFormat::TarGz),
			(1, "tool-linux-amd64.tar.gz", ArchiveFormat::TarGz),
			(2, "tool-darwin-arm64.tar.gz", ArchiveFormat::TarGz),
			(5, "tool-msvc32.zip", ArchiveFormat::Zip),
			(6, "tool-msvc64.zip", ArchiveFormat::Zip),
		];
		for (target, (index, name, format)) in targets.into_iter().zip(expected) {
			assert_eq!(select_with(names, &target), Some((index, name, format)), "{target:?}");
		}
	}

	#[test]
	fn selects_first_on_tie() {
		let target = Target {
			os: "linux",
			arch: "x86_64",
			libc: "gnu",
		};
		let names = ["tool-linux-amd64-b.tar.gz", "tool-linux-amd64-a.tar.gz"];
		assert_eq!(select_with(names, &target), Some((1, "tool-linux-amd64-a.tar.gz", ArchiveFormat::TarGz)));
	}

	#[test]
	fn rejects_unsupported_formats() {
		let targets = [
			Target {
				os: "linux",
				arch: "x86_64",
				libc: "gnu",
			},
			Target {
				os: "macos",
				arch: "aarch64",
				libc: "",
			},
			Target {
				os: "windows",
				arch: "x86_64",
				libc: "msvc",
			},
		];
		let names = [
			"tool-linux-amd64.deb",
			"tool-linux-amd64.rpm",
			"tool-darwin-arm64.pkg",
			"tool-windows-x86_64.msi",
			"tool-linux-amd64",
		];
		for target in targets {
			assert_eq!(select_with(names, &target), None, "{target:?}");
		}
	}

	#[test]
	fn rejects_excluded_assets() {
		let target = Target {
			os: "linux",
			arch: "x86_64",
			libc: "gnu",
		};
		let excluded = [
			"tool-linux-amd64.tar.gz.sha256",
			"tool-linux-amd64.tar.gz.sha512",
			"tool-linux-amd64-checksums.txt",
			"tool-linux-amd64.tar.gz.sig",
			"tool-linux-amd64.tar.gz.asc",
			"tool-linux-amd64.tar.gz.pem",
		];
		assert_eq!(select_with(excluded, &target), None);

		let mut names = excluded.to_vec();
		names.push("tool-linux-amd64.tar.gz");
		assert_eq!(select_with(names, &target), Some((6, "tool-linux-amd64.tar.gz", ArchiveFormat::TarGz)));
	}

	#[test]
	fn rejects_unmatched_os() {
		let target = Target {
			os: "linux",
			arch: "x86_64",
			libc: "gnu",
		};
		let names = ["tool-darwin-arm64.tar.gz", "tool-windows-x86_64.zip", "tool-msvc64.zip"];
		assert_eq!(select_with(names, &target), None);
	}

	#[test]
	fn rejects_foreign_os_with_libc_hint() {
		let target = Target {
			os: "linux",
			arch: "x86_64",
			libc: "gnu",
		};
		let names = ["tool-windows-gnu.zip", "tool-darwin-gnu.zip"];
		assert_eq!(select_with(names, &target), None);
	}

	#[test]
	fn rejects_win64_in_darwin64() {
		let windows = Target {
			os: "windows",
			arch: "x86_64",
			libc: "msvc",
		};
		let macos = Target {
			os: "macos",
			arch: "aarch64",
			libc: "",
		};
		let names = ["tool-darwin64-arm64.zip", "tool-darwin32-x86.zip"];
		assert_eq!(select_with(names, &windows), None);
		assert_eq!(select_with(names, &macos), Some((0, "tool-darwin64-arm64.zip", ArchiveFormat::Zip)));
	}

	#[test]
	fn rejects_foreign_arch() {
		let linux_x86_64 = Target {
			os: "linux",
			arch: "x86_64",
			libc: "gnu",
		};
		let names = ["tool-linux-arm64.tar.gz", "tool-linux-x86.tar.gz"];
		assert_eq!(select_with(names, &linux_x86_64), None);
		assert_eq!(
			select_with(["tool-linux.tar.gz"], &linux_x86_64),
			Some((0, "tool-linux.tar.gz", ArchiveFormat::TarGz))
		);

		let linux_x86 = Target {
			os: "linux",
			arch: "x86",
			libc: "gnu",
		};
		assert_eq!(
			select_with(["tool-linux-x86_64.tar.gz", "tool-linux-x86.tar.gz"], &linux_x86),
			Some((1, "tool-linux-x86.tar.gz", ArchiveFormat::TarGz))
		);

		let windows_x86 = Target {
			os: "windows",
			arch: "x86",
			libc: "msvc",
		};
		assert_eq!(
			select_with(["tool-win32-x64.zip", "tool-win32.zip"], &windows_x86),
			Some((1, "tool-win32.zip", ArchiveFormat::Zip))
		);
	}
}
