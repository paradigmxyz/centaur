//! The files capability: a directory connectors may read and write, such as
//! the agent's workspace in a Centaur sandbox. Off unless the runner is
//! started with `--files-dir`.
//!
//! Paths are relative to the root, or absolute paths inside it. Every path
//! must stay inside the root after symlinks resolve. Writes go to a temporary
//! file in the target directory and are renamed into place, so a write never
//! follows a symlink at the destination and a reader never sees a partial
//! file.

use std::{
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use eyre::{Result, WrapErr, bail, eyre};
use tokio::io::AsyncWriteExt;

/// Largest file a connector may read, write or download.
pub const MAX_FILE_BYTES: usize = 100 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Files {
    /// The root as configured, for matching absolute paths lexically.
    given: Arc<PathBuf>,
    /// The root with symlinks resolved; containment is checked against it.
    root: Arc<PathBuf>,
}

/// A file a host function read or wrote.
#[derive(Clone, Debug)]
pub struct FileInfo {
    /// Absolute path, as the agent's other tools would open it.
    pub path: PathBuf,
    pub bytes: u64,
}

impl Files {
    pub fn new(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)
            .wrap_err_with(|| format!("creating files dir {}", root.display()))?;
        let canonical = root
            .canonicalize()
            .wrap_err_with(|| format!("resolving files dir {}", root.display()))?;
        let given = if root.is_absolute() {
            root.to_owned()
        } else {
            canonical.clone()
        };
        Ok(Self {
            given: Arc::new(given),
            root: Arc::new(canonical),
        })
    }

    #[cfg(test)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path relative to the root, made only of normal components.
    fn relative(&self, path: &str) -> Result<PathBuf> {
        let candidate = Path::new(path);
        let relative = if candidate.is_absolute() {
            candidate
                .strip_prefix(self.root.as_path())
                .or_else(|_| candidate.strip_prefix(self.given.as_path()))
                .map_err(|_| {
                    eyre!(
                        "path is outside the files directory {}",
                        self.root.display()
                    )
                })?
        } else {
            candidate
        };
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|component| matches!(component, Component::Normal(_)))
        {
            bail!("path must name a file inside the files directory, without `.` or `..` segments");
        }
        Ok(relative.to_owned())
    }

    fn contained(&self, path: &Path) -> Result<()> {
        if path.starts_with(self.root.as_path()) {
            Ok(())
        } else {
            bail!("path resolves outside the files directory")
        }
    }

    /// An existing regular file, resolved and checked.
    async fn existing(&self, path: &str) -> Result<(PathBuf, std::fs::Metadata)> {
        let full = tokio::fs::canonicalize(self.root.join(self.relative(path)?))
            .await
            .wrap_err("no such file")?;
        self.contained(&full)?;
        let metadata = tokio::fs::metadata(&full).await?;
        if !metadata.is_file() {
            bail!("not a regular file");
        }
        Ok((full, metadata))
    }

    pub async fn read(&self, path: &str) -> Result<(FileInfo, Vec<u8>)> {
        let (full, metadata) = self.existing(path).await?;
        if metadata.len() > MAX_FILE_BYTES as u64 {
            bail!("file is over the {MAX_FILE_BYTES}-byte limit");
        }
        let data = tokio::fs::read(&full).await?;
        Ok((
            FileInfo {
                path: full,
                bytes: data.len() as u64,
            },
            data,
        ))
    }

    /// `None` when nothing exists at the path.
    pub async fn stat(&self, path: &str) -> Result<Option<FileInfo>> {
        let full = self.root.join(self.relative(path)?);
        if tokio::fs::symlink_metadata(&full).await.is_err() {
            return Ok(None);
        }
        let (full, metadata) = self.existing(path).await?;
        Ok(Some(FileInfo {
            path: full,
            bytes: metadata.len(),
        }))
    }

    pub async fn write(&self, path: &str, data: &[u8]) -> Result<FileInfo> {
        if data.len() > MAX_FILE_BYTES {
            bail!("data is over the {MAX_FILE_BYTES}-byte limit");
        }
        let mut writer = self.create(path).await?;
        writer.write(data).await?;
        writer.finish().await
    }

    /// Start writing a file. Nothing appears at `path` until `finish`; a
    /// dropped writer removes its temporary file.
    pub async fn create(&self, path: &str) -> Result<Writer> {
        let relative = self.relative(path)?;
        let parent = self
            .directory(relative.parent().unwrap_or(Path::new("")))
            .await?;
        let name = relative
            .file_name()
            .ok_or_else(|| eyre!("path has no file name"))?
            .to_owned();
        let target = parent.join(&name);
        if tokio::fs::metadata(&target)
            .await
            .is_ok_and(|metadata| metadata.is_dir())
        {
            bail!("path is a directory");
        }
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temporary = parent.join(format!(
            ".{}.{}-{}.part",
            name.to_string_lossy(),
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .await
            .wrap_err("creating file")?;
        Ok(Writer {
            file: Some(file),
            temporary,
            target,
            bytes: 0,
        })
    }

    /// Create `relative` under the root one component at a time, refusing any
    /// component that resolves outside it, and return it resolved.
    async fn directory(&self, relative: &Path) -> Result<PathBuf> {
        let mut current = self.root.as_path().to_owned();
        for component in relative.components() {
            let next = current.join(component);
            match tokio::fs::symlink_metadata(&next).await {
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match tokio::fs::create_dir(&next).await {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error).wrap_err("creating directory"),
                    }
                }
                Err(error) => return Err(error.into()),
            }
            current = tokio::fs::canonicalize(&next).await?;
            self.contained(&current)?;
            if !tokio::fs::metadata(&current).await?.is_dir() {
                bail!("{} is not a directory", next.display());
            }
        }
        Ok(current)
    }
}

pub struct Writer {
    file: Option<tokio::fs::File>,
    temporary: PathBuf,
    target: PathBuf,
    bytes: u64,
}

impl Writer {
    pub async fn write(&mut self, chunk: &[u8]) -> Result<()> {
        self.bytes += chunk.len() as u64;
        if self.bytes > MAX_FILE_BYTES as u64 {
            bail!("file is over the {MAX_FILE_BYTES}-byte limit");
        }
        let file = self.file.as_mut().expect("writer is open");
        file.write_all(chunk).await?;
        Ok(())
    }

    pub async fn finish(mut self) -> Result<FileInfo> {
        let mut file = self.file.take().expect("writer is open");
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&self.temporary, &self.target)
            .await
            .wrap_err("moving file into place")?;
        Ok(FileInfo {
            path: self.target.clone(),
            bytes: self.bytes,
        })
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.temporary);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stays_inside_the_root() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::new(&dir.path().join("root")).unwrap();
        let info = files.write("a/b.txt", b"hi").await.unwrap();
        assert_eq!(info.path, files.root().join("a/b.txt"));
        assert_eq!(files.read("a/b.txt").await.unwrap().1, b"hi");
        let absolute = files.root().join("a/b.txt");
        assert_eq!(
            files.read(absolute.to_str().unwrap()).await.unwrap().1,
            b"hi"
        );
        assert!(files.stat("missing").await.unwrap().is_none());

        for path in ["", "../x", "a/../../x", "/etc/hostname", "./a/b.txt"] {
            assert!(files.read(path).await.is_err(), "read {path:?}");
            assert!(files.write(path, b"x").await.is_err(), "write {path:?}");
        }

        std::os::unix::fs::symlink(dir.path(), files.root().join("escape")).unwrap();
        assert!(files.write("escape/x.txt", b"x").await.is_err());
        assert!(!dir.path().join("x.txt").exists());
        std::fs::write(dir.path().join("secret"), "s").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), files.root().join("link")).unwrap();
        assert!(files.read("link").await.is_err());
        // Writing replaces the link itself rather than following it.
        files.write("link", b"new").await.unwrap();
        assert_eq!(std::fs::read(dir.path().join("secret")).unwrap(), b"s");
    }

    #[tokio::test]
    async fn dropped_writers_leave_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let files = Files::new(dir.path()).unwrap();
        let mut writer = files.create("out.bin").await.unwrap();
        writer.write(b"partial").await.unwrap();
        drop(writer);
        assert_eq!(std::fs::read_dir(files.root()).unwrap().count(), 0);
    }
}
