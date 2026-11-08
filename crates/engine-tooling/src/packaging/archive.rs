//! Deterministic zip and tar.gz writers over a staged package directory.
//!
//! Entries are sorted by path, directories come before their contents, every
//! entry has a fixed modification time and no owner, directories and the files
//! named in `executables` (paths relative to the package root, `/` separated) are mode 0755 and every other file is
//! 0644. The modes are written explicitly, so an archive built on Windows keeps
//! the executable bit when extracted on Linux or macOS.

use super::PackageFormat;
use engine_project::{Error, Result};
use std::fs::{self, File};
use std::io::{BufWriter, Seek, Write};
use std::path::{Path, PathBuf};

/// 2020-01-01T00:00:00Z. Zip cannot store dates before 1980.
const FIXED_UNIX_TIME: u64 = 1_577_836_800;
const DIRECTORY_MODE: u32 = 0o755;
const EXECUTABLE_MODE: u32 = 0o755;
const FILE_MODE: u32 = 0o644;

struct Entry {
    /// Forward-slash path below the top-level folder; empty for the folder itself.
    relative: String,
    source: PathBuf,
    directory: bool,
    mode: u32,
}

/// Populate a staging directory with `populate`, archive it as `folder/...` and
/// publish the archive at `destination` only when complete. An existing
/// destination is refused before and again when publishing.
pub fn export_new_archive(
    destination: &Path,
    folder: &str,
    format: PackageFormat,
    executables: &[&str],
    populate: impl FnOnce(&Path) -> Result<()>,
) -> Result<()> {
    if destination.exists() {
        return Err(exists());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()?;
    let staging = tempfile::Builder::new()
        .prefix(".engine-export-")
        .tempdir_in(&parent)?;
    populate(staging.path())?;
    let entries = collect(staging.path(), executables)?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".engine-export-")
        .suffix(".partial")
        .tempfile_in(&parent)?;
    {
        let writer = BufWriter::new(temporary.as_file_mut());
        match format {
            PackageFormat::Zip => write_zip(writer, folder, &entries)?,
            PackageFormat::TarGz => write_tar_gz(writer, folder, &entries)?,
            PackageFormat::Dir
            | PackageFormat::AppImage
            | PackageFormat::App
            | PackageFormat::SteamDepot => {
                return Err(Error::new(
                    "package.format",
                    "Only zip and tar.gz are archive formats",
                ));
            }
        }
    }
    // A temporary file is created 0600; published archives are world-readable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(fs::Permissions::from_mode(FILE_MODE))?;
    }
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(destination).map_err(|error| {
        if destination.exists() {
            exists()
        } else {
            Error::from(error.error)
        }
    })?;
    Ok(())
}

fn exists() -> Error {
    Error::new("tooling.destination.exists", "Destination must be new")
}

fn collect(root: &Path, executables: &[&str]) -> Result<Vec<Entry>> {
    let mut entries = vec![Entry {
        relative: String::new(),
        source: root.to_path_buf(),
        directory: true,
        mode: DIRECTORY_MODE,
    }];
    walk(root, "", executables, &mut entries)?;
    Ok(entries)
}

fn walk(directory: &Path, prefix: &str, executables: &[&str], out: &mut Vec<Entry>) -> Result<()> {
    let mut children = fs::read_dir(directory)?
        .map(|entry| entry.map_err(Error::from))
        .collect::<Result<Vec<_>>>()?;
    let mut named = Vec::with_capacity(children.len());
    for child in children.drain(..) {
        let name = child
            .file_name()
            .into_string()
            .map_err(|_| Error::new("tooling.path", "Package file names must be valid UTF-8"))?;
        named.push((name, child));
    }
    named.sort_by(|a, b| a.0.cmp(&b.0));
    for (file_name, child) in named {
        let relative = if prefix.is_empty() {
            file_name.clone()
        } else {
            format!("{prefix}/{file_name}")
        };
        let kind = child.file_type()?;
        if kind.is_dir() {
            out.push(Entry {
                relative: relative.clone(),
                source: child.path(),
                directory: true,
                mode: DIRECTORY_MODE,
            });
            walk(&child.path(), &relative, executables, out)?;
        } else if kind.is_file() {
            let executable = executables.contains(&relative.as_str());
            out.push(Entry {
                relative,
                source: child.path(),
                directory: false,
                mode: if executable {
                    EXECUTABLE_MODE
                } else {
                    FILE_MODE
                },
            });
        } else {
            return Err(Error::new(
                "tooling.path",
                "Packages hold only regular files and directories",
            ));
        }
    }
    Ok(())
}

fn entry_path(folder: &str, entry: &Entry) -> String {
    if entry.relative.is_empty() {
        folder.to_owned()
    } else {
        format!("{folder}/{}", entry.relative)
    }
}

fn write_tar_gz(sink: impl Write, folder: &str, entries: &[Entry]) -> Result<()> {
    let encoder = flate2::GzBuilder::new()
        .mtime(0)
        .write(sink, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);
    for entry in entries {
        let mut header = tar::Header::new_gnu();
        header.set_mode(entry.mode);
        header.set_mtime(FIXED_UNIX_TIME);
        header.set_uid(0);
        header.set_gid(0);
        let path = entry_path(folder, entry);
        if entry.directory {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            builder.append_data(&mut header, format!("{path}/"), std::io::empty())?;
        } else {
            let file = File::open(&entry.source)?;
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(file.metadata()?.len());
            builder.append_data(&mut header, path, file)?;
        }
    }
    builder.into_inner()?.finish()?.flush()?;
    Ok(())
}

fn write_zip(sink: impl Write + Seek, folder: &str, entries: &[Entry]) -> Result<()> {
    use zip::{CompressionMethod, DateTime, write::SimpleFileOptions};
    let archive = |error: zip::result::ZipError| Error::new("package.archive", error.to_string());
    let modified = DateTime::from_date_and_time(2020, 1, 1, 0, 0, 0)
        .map_err(|_| Error::new("package.archive", "Invalid fixed archive time"))?;
    let mut writer = zip::ZipWriter::new(sink);
    for entry in entries {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            // Without this a Windows host writes DOS attributes and drops the modes.
            .system(zip::System::Unix)
            .last_modified_time(modified)
            .unix_permissions(entry.mode);
        let path = entry_path(folder, entry);
        if entry.directory {
            writer.add_directory(path, options).map_err(archive)?;
        } else {
            let mut file = File::open(&entry.source)?;
            // Entries of 4 GiB or more need zip64 headers.
            let options = options.large_file(file.metadata()?.len() >= u64::from(u32::MAX));
            writer.start_file(path, options).map_err(archive)?;
            std::io::copy(&mut file, &mut writer)?;
        }
    }
    writer.finish().map_err(archive)?.flush()?;
    Ok(())
}
