//! 按业务 locator 加载 TOML 配置，并在业务校验后补齐文件。
mod document;
use anyhow::{Context, Result};
use config_contract::RootConfig;
use document::{ConfigDocument, PendingConfig};
use fs2::FileExt;
use std::{
    ffi::OsString,
    fs::{self, OpenOptions},
    io::ErrorKind,
    path::{self, Path, PathBuf},
};

pub struct ConfigFile {
    path: PathBuf,
    pending: Vec<PendingConfig>,
}
impl ConfigFile {
    pub fn open(explicit: Option<&Path>) -> Result<Self> {
        let requested = match explicit {
            Some(path) => path::absolute(path)?,
            None => std::env::home_dir()
                .context("could not locate home directory; pass --config <path>")?
                .join(".actrail/actrail-kv/settings.toml"),
        };
        let path = match requested.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == ErrorKind::NotFound => requested,
            Err(error) => return Err(error).context("failed to resolve settings path"),
        };
        anyhow::ensure!(
            path.file_name().is_some(),
            "settings path needs a file name"
        );
        Ok(Self {
            path,
            pending: Vec::new(),
        })
    }

    pub fn read<C: RootConfig>(&mut self, locator: &[&str]) -> Result<C> {
        let config = C::load(&self.path, locator)
            .with_context(|| format!("invalid configuration {}", self.path.display()))?;
        self.pending.push(PendingConfig::new::<C>(
            locator,
            config.try_to_toml_table()?,
        ));
        Ok(config)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn directory(&self) -> &Path {
        self.path
            .parent()
            .expect("absolute configuration path has a parent")
    }
    pub fn resolve(&self, path: impl AsRef<Path>) -> PathBuf {
        self.directory().join(path)
    }
    pub fn check_output(&self, path: &Path) -> Result<()> {
        let output = path::absolute(path)?;
        let same_file = match same_file::is_same_file(&self.path, &output) {
            Ok(same) => same,
            Err(error) if error.kind() == ErrorKind::NotFound => false,
            Err(error) => return Err(error).context("failed to check settings and output paths"),
        };
        let canonical_entry = match (output.parent(), output.file_name()) {
            (Some(parent), Some(name)) => {
                parent.canonicalize().ok().map(|parent| parent.join(name))
            }
            _ => None,
        };
        anyhow::ensure!(
            output != self.path && !same_file && canonical_entry.as_ref() != Some(&self.path),
            "output must not overwrite settings file {}",
            self.path.display()
        );
        Ok(())
    }

    pub fn finish(self) -> Result<PathBuf> {
        if self.pending.is_empty() {
            return Ok(self.path);
        }
        fs::create_dir_all(self.directory())?;
        let mut lock_name = OsString::from(".");
        lock_name.push(self.path.file_name().expect("validated configuration path"));
        lock_name.push(".lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock = options.open(self.directory().join(lock_name))?;
        FileExt::lock_exclusive(&lock)?;
        // 拿锁后重读，保留其他进程已写入的配置值。
        let mut document = ConfigDocument::load(&self.path, &[])?;
        let mut changed = false;
        for pending in self.pending {
            changed |= document.complete(pending)?;
        }
        if changed {
            document.update(&self.path, &[])?;
        }
        Ok(self.path)
    }
}
