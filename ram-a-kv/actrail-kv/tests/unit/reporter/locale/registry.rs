//! 本文件验证 locale/registry 模块的行为。

#[test]
fn all_locales_have_identical_namespace_and_key_sets() {
    super::validate_catalogs().expect("valid locale catalogs");
}
