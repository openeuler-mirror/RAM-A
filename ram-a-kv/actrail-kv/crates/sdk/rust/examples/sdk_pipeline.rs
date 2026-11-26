//! 演示持续采集、按需分析、直接内存渲染和显式报告导出。
use actrail_kv_sdk::*;
use serde_json::json;
use std::path::PathBuf;

fn main() -> Result<()> {
    // 两个可选命令行参数：动态库目录、报告导出目录，默认分别为 target/release 和 sdk-data。
    // canonicalize() 将目录转为绝对路径，末尾的 ? 表示失败时返回错误并结束示例。
    let mut args = std::env::args_os().skip(1);
    let libraries =
        PathBuf::from(args.next().unwrap_or_else(|| "target/release".into())).canonicalize()?;
    let directory = PathBuf::from(args.next().unwrap_or_else(|| "sdk-data".into()));
    std::fs::create_dir_all(&directory)?;
    let directory = directory.canonicalize()?;
    // create 加载指定动态库并创建实例。receiver 通过函数接收请求，不启动 HTTP 服务。
    let mut receiver = ReceiverFactory::create(SdkConfig {
        library_path: libraries.join("libactrail_receiver.so"),
        // 显式导出的相对路径以此目录为基准。文件配置内的路径以 settings.toml 所在目录为基准。
        base_dir: directory.clone(),
        // None 读取 ~/.actrail/actrail-kv/settings.toml，Some(text) 使用传入的 TOML。
        config_toml: None,
        overrides: SdkOverrides::default(),
    })?;
    // 模拟四条请求：前面的工作区名称变化，后面有相同的长指令，用来展示前缀复用障碍。
    // submit 成功返回时，该请求已写入采集存储。
    for workspace in ["billing", "search", "orders", "accounts"] {
        receiver.submit(&CaptureInput {
            payload: json!({"model":"example", "messages":[
                {"role":"system", "content":format!("Workspace: {workspace}. Follow the repository rules, read the code before making changes, and run the relevant tests before completion.")},
                {"role":"user", "content":"Fix the failing validation test."}
            ]}), source: Some("sdk-example".into()), session_key: Some("example".into()),
            comparison: CaptureComparison { endpoint_key: "example-model".into(), agent_key: None, model_deployment_key: None, kv_namespace: None },
        })?;
    }
    // 加载 libactrail_analyzer.so，按默认筛选设置读取上述采集存储，返回内存分析结果。
    let mut analyzer = AnalyzerFactory::create(SdkConfig {
        library_path: libraries.join("libactrail_analyzer.so"),
        base_dir: directory.clone(),
        config_toml: None,
        overrides: SdkOverrides::default(),
    })?;
    let selection = Selection {
        // None 沿用实例配置。指定会话可写 Some(vec!["example".into()])。
        session_keys: None,
        // None 沿用实例配置。指定时间段可传 Some(vec![TimeRange { from, until }])。
        time_ranges: None,
        // Inherit 沿用配置中的数量限制，All 清除限制，Last(n) 只取最近 n 条。
        last_n: RecordLimit::Inherit,
    };
    let analysis = analyzer.analyze(&selection)?;
    // 关闭分析实例后，analysis 仍然有效，可继续传给报告组件。
    analyzer.close()?;
    // 加载 libactrail_reporter.so，将内存中的 analysis 直接渲染为 HTML，无需中间 JSON 文件。
    let mut reporter = ReporterFactory::create(SdkConfig {
        library_path: libraries.join("libactrail_reporter.so"),
        base_dir: directory.clone(),
        config_toml: None,
        // 只覆盖语言，报告格式等未设置字段继续使用配置文件中的值。
        overrides: SdkOverrides {
            report: Some(ReportSettingsOverrides {
                locale: Some("zh-CN".into()),
                ..Default::default()
            }),
            ..Default::default()
        },
    })?;
    let report = reporter.render(
        &analysis,
        &RenderOptions {
            format: Some(ReportFormat::Html),
            locale: None, // 沿用创建实例时覆盖后的语言设置。
        },
    )?;
    reporter.close()?;
    receiver.close()?;
    // 处理完成后仍可读取或显式导出，关闭实例不释放其输出。
    println!(
        "rendered {} bytes in memory",
        report.reports()?[0].content.len()
    );
    // render 只生成内存报告，此处才写入文件，相对路径以 base_dir（数据目录）为基准。
    report.export("report.html")?;
    println!("exported {}", directory.join("report.html").display());
    Ok(())
}
