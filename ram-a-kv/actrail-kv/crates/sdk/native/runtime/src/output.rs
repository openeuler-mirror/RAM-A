//! 将显式选择的产物原子发布到指定绝对路径。
use crate::{Error, Result};
use serde_json::{json, Value};
use std::{fs::File, io::Write, path::Path};

pub fn publish(
    destination: &[u8],
    bytes: &[u8],
    source: Option<&crate::SourceProtection>,
) -> Result<Value> {
    let request = crate::request(destination)?;
    let path = request
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| Error::input("export path is required"))?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(Error::input("export path must be absolute"));
    }
    if let Some(source) = source {
        source.validate_output(path)?;
    }
    let parent = path
        .parent()
        .ok_or_else(|| Error::input("export path must name a file"))?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(Error::storage)?;
    file.write_all(bytes).map_err(Error::storage)?;
    file.as_file_mut().sync_all().map_err(Error::storage)?;
    file.persist(path).map_err(Error::storage)?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|file| file.sync_all())
        .map_err(Error::storage)?;
    Ok(json!({"path": path}))
}
