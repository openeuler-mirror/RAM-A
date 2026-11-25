//! 验证多个业务 locator 的并发补全、原值保留与文件失败保护。
use actrail_kv_settings::ConfigFile;
use config_contract::{toml_edit::DocumentMut, Config, RootConfig};
use serde::Serialize;
use std::{
    fs,
    sync::{Arc, Barrier},
    thread,
};

#[derive(Default, Serialize, Config)]
#[serde(default)]
struct OutputConfig {
    /// 输出位置
    #[serde(alias = "legacy_path")]
    path: String,
    /// 输出记录上限
    limit: usize,
}
impl RootConfig for OutputConfig {}

#[test]
fn concurrent_commands_merge_defaults_without_losing_other_sections() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("new/settings.toml");
    let ready = Arc::new(Barrier::new(2));
    let workers: Vec<_> = ["receiver", "report"]
        .into_iter()
        .map(|name| {
            let path = path.clone();
            let ready = ready.clone();
            thread::spawn(move || {
                let mut file = ConfigFile::open(Some(&path)).unwrap();
                let _: OutputConfig = file.read(&[name]).unwrap();
                ready.wait();
                file.finish().unwrap();
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let completed: DocumentMut = fs::read_to_string(path).unwrap().parse().unwrap();
    assert!(completed.contains_key("receiver"));
    assert!(completed.contains_key("report"));
}

#[test]
fn completes_only_missing_fields_and_leaves_invalid_settings_intact() {
    let temporary = tempfile::tempdir().unwrap();
    let path = temporary.path().join("settings.toml");
    let original = "[export]\n# user path\nlegacy_path = 'custom.ndjson'\n[other]\nvalue = true\n";
    fs::write(&path, original).unwrap();
    let mut file = ConfigFile::open(Some(&path)).unwrap();
    let config: OutputConfig = file.read(&["export"]).unwrap();
    assert_eq!(config.path, "custom.ndjson");
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    file.finish().unwrap();
    let completed = fs::read_to_string(&path).unwrap();
    assert!(completed.contains("# user path\nlegacy_path = 'custom.ndjson'"));
    assert!(completed.contains("# 输出记录上限\nlimit = 0"));
    assert!(completed.contains("[other]\nvalue = true"));
    let mut file = ConfigFile::open(Some(&path)).unwrap();
    let _: OutputConfig = file.read(&["export"]).unwrap();
    file.finish().unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), completed);
    let invalid = "[export\n";
    fs::write(&path, invalid).unwrap();
    let mut file = ConfigFile::open(Some(&path)).unwrap();
    assert!(file.read::<OutputConfig>(&["export"]).is_err());
    assert_eq!(fs::read_to_string(path).unwrap(), invalid);
}
