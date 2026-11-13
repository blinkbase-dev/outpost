#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ArchiveFormat {
	TarGz,
	TarXz,
	Zip,
}

impl ArchiveFormat {
	pub(crate) fn from_name(name: &str) -> Option<Self> {
		let name = name.to_ascii_lowercase();
		if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
			Some(Self::TarGz)
		} else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
			Some(Self::TarXz)
		} else if name.ends_with(".zip") {
			Some(Self::Zip)
		} else {
			None
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn detects_archive_formats() {
		assert_eq!(ArchiveFormat::from_name("tool-linux.tar.gz"), Some(ArchiveFormat::TarGz));
		assert_eq!(ArchiveFormat::from_name("tool-linux.tgz"), Some(ArchiveFormat::TarGz));
		assert_eq!(ArchiveFormat::from_name("tool-linux.tar.xz"), Some(ArchiveFormat::TarXz));
		assert_eq!(ArchiveFormat::from_name("tool-linux.txz"), Some(ArchiveFormat::TarXz));
		assert_eq!(ArchiveFormat::from_name("tool-windows.zip"), Some(ArchiveFormat::Zip));
		assert_eq!(ArchiveFormat::from_name("tool-linux.deb"), None);
	}
}
