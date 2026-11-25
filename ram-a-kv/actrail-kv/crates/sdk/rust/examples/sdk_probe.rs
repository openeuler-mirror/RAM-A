//! 加载指定分析库并打印结果，用于验证同一宿主的动态库独立替换。
use actrail_kv_sdk::{AnalyzerFactory, SdkConfig, Selection};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let library = std::env::args_os()
        .nth(1)
        .ok_or("expected analyzer library path")?;
    let mut analyzer = AnalyzerFactory::create(SdkConfig {
        library_path: library.into(),
        base_dir: std::env::current_dir()?,
        config_toml: Some(String::new()),
        overrides: Default::default(),
    })?;
    let output = analyzer.analyze(&Selection::default())?;
    analyzer.close()?;
    println!("{}", output.json()?.get());
    Ok(())
}
