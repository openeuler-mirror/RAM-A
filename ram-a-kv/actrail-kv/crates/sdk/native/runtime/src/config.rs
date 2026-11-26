//! 从内存 TOML 或默认配置文件读取所属模块，并固定配置路径基准。
use crate::{Error, Result};
use actrail_kv_settings::ConfigFile;
use config_contract::{
    toml_edit::{DocumentMut, Item, Table, TableLike},
    RootConfig,
};
use std::path::{Path, PathBuf};

pub struct Config {
    document: DocumentMut,
    file: Option<ConfigFile>,
    overrides: Table,
    pub base_dir: PathBuf,
}
impl Config {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = crate::request(bytes)?;
        let base_dir = value
            .get("base_dir")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::config("base_dir is required"))?;
        let base_dir = PathBuf::from(base_dir);
        if !base_dir.is_absolute() {
            return Err(Error::config("base_dir must be absolute"));
        }
        let overrides = config_contract::toml_edit::ser::to_document(
            value.get("overrides").unwrap_or(&serde_json::json!({})),
        )
        .map_err(Error::config)?
        .as_table()
        .clone();
        match value.get("config_toml") {
            None | Some(serde_json::Value::Null) => {
                let file = ConfigFile::open(None).map_err(Error::config)?;
                Ok(Self {
                    base_dir: file.directory().to_owned(),
                    document: DocumentMut::new(),
                    file: Some(file),
                    overrides,
                })
            }
            Some(serde_json::Value::String(text)) => Ok(Self {
                document: text.parse().map_err(Error::config)?,
                file: None,
                overrides,
                base_dir,
            }),
            Some(_) => Err(Error::config("config_toml must be a string or null")),
        }
    }
    pub fn section<T: RootConfig>(&mut self, name: &str) -> Result<T> {
        if let Some(file) = &mut self.file {
            // 缓存基础配置，供 finish 后使用；文件补全不包含本次覆盖值。
            let base: T = file.read(&[name]).map_err(Error::config)?;
            self.document.insert(
                name,
                Item::Table(base.try_to_toml_table().map_err(Error::config)?),
            );
        }
        let empty = Table::new();
        let table = match self.document.get(name) {
            Some(item) => item
                .as_table()
                .ok_or_else(|| Error::config(format!("{name} must be a TOML table")))?,
            None => &empty,
        };
        let mut table = table.clone();
        if let Some(overrides) = self.overrides.get(name).and_then(Item::as_table_like) {
            merge(&mut table, overrides);
        }
        T::parse_toml_table(&table)
            .map(|result| result.0)
            .map_err(Error::config)
    }
    /// 所属模块完成配置读取后，复用 CLI 的加锁补全与写回流程。
    pub fn finish(&mut self) -> Result<()> {
        if let Some(file) = self.file.take() {
            file.finish().map_err(Error::config)?;
        }
        Ok(())
    }
    pub fn path(&self, path: &Path) -> PathBuf {
        if path.is_absolute() {
            path.to_owned()
        } else {
            self.base_dir.join(path)
        }
    }
}

// 表按字段递归覆盖，数组和标量整体替换，未出现的字段保持原值。
fn merge(target: &mut dyn TableLike, overrides: &dyn TableLike) {
    for (key, value) in overrides.iter() {
        if let (Some(target), Some(source)) = (
            target.get_mut(key).and_then(Item::as_table_like_mut),
            value.as_table_like(),
        ) {
            merge(target, source);
        } else {
            target.insert(key, value.clone());
        }
    }
}
