//! 本文件注册各 locale 的 namespace 子表并执行 key 查询。

use anyhow::{bail, Result};

use super::{catalogs, namespace, Locale};

pub(super) struct NamespaceCatalog {
    pub name: &'static str,
    pub entries: &'static [(&'static str, &'static str)],
}

pub(super) fn namespaces(locale: Locale) -> &'static [NamespaceCatalog] {
    match locale {
        Locale::ZhCn => catalogs::zh_cn::catalog(),
        Locale::EnUs => catalogs::en_us::catalog(),
    }
}

pub(super) fn lookup(locale: Locale, module: &str, key: &str) -> Result<&'static str> {
    let namespace = namespace_catalog(locale, module)?;
    namespace
        .entries
        .iter()
        .find_map(|(candidate, value)| (*candidate == key).then_some(*value))
        .ok_or_else(|| anyhow::anyhow!("missing locale key {module}.{key}"))
}

pub(super) fn namespace_catalog(locale: Locale, module: &str) -> Result<&'static NamespaceCatalog> {
    namespaces(locale)
        .iter()
        .find(|namespace| namespace.name == module)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "unregistered locale namespace {module}; registered namespaces: {}",
                namespace::ALL.join(", ")
            )
        })
}

pub(super) fn validate_catalogs() -> Result<()> {
    let expected = namespace::ALL;
    for locale in [Locale::ZhCn, Locale::EnUs] {
        let catalog = namespaces(locale);
        if catalog.len() != expected.len() {
            bail!(
                "locale {} registers {} namespaces, expected {}",
                locale.code(),
                catalog.len(),
                expected.len()
            );
        }
        for (namespace, expected_name) in catalog.iter().zip(expected) {
            if namespace.name != *expected_name {
                bail!(
                    "locale {} namespace order mismatch: {} != {}",
                    locale.code(),
                    namespace.name,
                    expected_name
                );
            }
            for index in 0..namespace.entries.len() {
                let key = namespace.entries[index].0;
                if key.is_empty()
                    || namespace.entries[..index]
                        .iter()
                        .any(|(candidate, _)| *candidate == key)
                {
                    bail!(
                        "locale {} namespace {} has empty or duplicate key {key}",
                        locale.code(),
                        namespace.name
                    );
                }
            }
        }
    }
    let zh = namespaces(Locale::ZhCn);
    let en = namespaces(Locale::EnUs);
    for (zh_namespace, en_namespace) in zh.iter().zip(en) {
        let mut zh_keys: Vec<_> = zh_namespace.entries.iter().map(|entry| entry.0).collect();
        let mut en_keys: Vec<_> = en_namespace.entries.iter().map(|entry| entry.0).collect();
        zh_keys.sort_unstable();
        en_keys.sort_unstable();
        if zh_keys != en_keys {
            bail!(
                "locale namespace {} key sets differ between zh-CN and en-US",
                zh_namespace.name
            );
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../../tests/unit/reporter/locale/registry.rs"]
mod tests;
