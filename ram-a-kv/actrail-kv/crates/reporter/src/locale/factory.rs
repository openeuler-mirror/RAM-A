//! 本文件管理进程级当前 locale，并提供不可变快照创建入口。

use std::sync::{OnceLock, RwLock};

use anyhow::{bail, Result};

use super::{namespace, Locale, LocaleModule, LocaleSnapshot};

static CURRENT_LOCALE: OnceLock<RwLock<Locale>> = OnceLock::new();

pub struct LocaleFactory;

impl LocaleFactory {
    /// 初始化全局 locale；未初始化时默认使用 zh-CN。
    pub fn init(locale: Locale) -> Result<()> {
        super::registry::validate_catalogs()?;
        Self::set_locale(locale)
    }

    /// 设置后续快照使用的 locale，不改变已创建快照。
    pub fn set_locale(locale: Locale) -> Result<()> {
        let current = CURRENT_LOCALE.get_or_init(|| RwLock::new(Locale::ZhCn));
        let mut guard = current
            .write()
            .map_err(|_| anyhow::anyhow!("locale registry lock is poisoned"))?;
        *guard = locale;
        Ok(())
    }

    pub fn current() -> Result<Locale> {
        let current = CURRENT_LOCALE.get_or_init(|| RwLock::new(Locale::ZhCn));
        current
            .read()
            .map(|guard| *guard)
            .map_err(|_| anyhow::anyhow!("locale registry lock is poisoned"))
    }

    /// 创建实例级快照，不读写进程全局语言设置。
    pub fn snapshot_for(locale: Locale) -> Result<LocaleSnapshot> {
        super::registry::validate_catalogs()?;
        Ok(LocaleSnapshot::new(locale))
    }

    pub fn snapshot() -> Result<LocaleSnapshot> {
        Ok(LocaleSnapshot::new(Self::current()?))
    }

    pub fn create(namespace: &str) -> Result<LocaleModule> {
        Self::snapshot()?.create(namespace)
    }

    pub fn init_from_code(code: &str) -> Result<Locale> {
        let Some(locale) = Locale::parse(code) else {
            let message = Self::create(namespace::CLI_REPORT)?.format(
                "unsupported_locale",
                &[
                    ("locale", code.to_string()),
                    ("supported", Locale::supported_codes().join(", ")),
                ],
            )?;
            bail!("{message}");
        };
        Self::init(locale)?;
        Ok(locale)
    }
}

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/locale/factory.rs"]
mod tests;
