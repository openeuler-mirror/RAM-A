//! 从默认业务配置生成包含字段文档注释的 TOML 示例。
use actrail_kv_analyzer::config::settings::AnalyzerSettings;
use config_contract::{
    toml_edit::{DocumentMut, Item},
    Config,
};

fn main() -> Result<(), config_contract::ConfigError> {
    let mut document = DocumentMut::new();
    document["analyzer"] = Item::Table(AnalyzerSettings::default().try_to_toml_table()?);
    print!("{document}");
    Ok(())
}
