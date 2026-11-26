//! 从默认业务配置生成包含字段文档注释的 TOML 示例。
use actrail_kv_receiver::config::settings::ReceiverSettings;
use config_contract::{
    toml_edit::{DocumentMut, Item},
    Config,
};

fn main() -> Result<(), config_contract::ConfigError> {
    let mut document = DocumentMut::new();
    document["receiver"] = Item::Table(ReceiverSettings::default().try_to_toml_table()?);
    print!("{document}");
    Ok(())
}
