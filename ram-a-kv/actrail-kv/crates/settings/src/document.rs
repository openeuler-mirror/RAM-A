//! 在原 TOML 文档上补齐业务配置缺项并保留已有值及注释。
use config_contract::{
    toml_edit::{Item, TableLike},
    Config, ConfigError, ConfigState, RootConfig, Table,
};

pub(super) struct PendingConfig {
    locator: Vec<String>,
    defaults: Table,
    prepare: fn(&dyn TableLike, &mut dyn TableLike),
}
impl PendingConfig {
    pub(super) fn new<C: Config>(locator: &[&str], defaults: Table) -> Self {
        Self {
            locator: locator.iter().map(|key| (*key).to_owned()).collect(),
            defaults,
            prepare: C::prepare_defaults,
        }
    }
}

pub(super) struct ConfigDocument(Table);
impl Config for ConfigDocument {
    fn parse_toml_table(table: &Table) -> Result<(Self, ConfigState), ConfigError> {
        Ok((Self(table.clone()), ConfigState::Clean))
    }
    fn to_toml_table(&self) -> Table {
        self.0.clone()
    }
}
impl RootConfig for ConfigDocument {}
impl ConfigDocument {
    pub(super) fn complete(&mut self, mut pending: PendingConfig) -> Result<bool, ConfigError> {
        let mut table = &mut self.0;
        let mut changed = false;
        for key in &pending.locator {
            if !table.contains_key(key) {
                table.insert(key, Item::Table(Table::new()));
                changed = true;
            }
            table = table
                .get_mut(key)
                .and_then(Item::as_table_mut)
                .ok_or_else(|| ConfigError::ExpectedTable(pending.locator.join(".")))?;
        }
        (pending.prepare)(table, &mut pending.defaults);
        Ok(
            config_contract::derive_support::fill_missing_fields(table, &pending.defaults)
                || changed,
        )
    }
}
