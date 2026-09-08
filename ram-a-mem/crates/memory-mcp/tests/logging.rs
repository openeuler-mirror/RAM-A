use std::process::Command;

fn run_with(name: &str, value: &str) -> std::process::Output {
    let directory = tempfile::tempdir().expect("temporary directory");
    Command::new(env!("CARGO_BIN_EXE_ram-a-mem"))
        .current_dir(directory.path())
        .env_remove("RAM_A_LOG_FORMAT")
        .env_remove("RAM_A_LOG_SOURCE")
        .env(name, value)
        .output()
        .expect("run ram-a-mem")
}

#[test]
fn invalid_log_format_fails_before_service_configuration() {
    for invalid in ["", "JSON", " compact", "text"] {
        let output = run_with("RAM_A_LOG_FORMAT", invalid);
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
        assert!(stderr.contains("invalid RAM_A_LOG_FORMAT"), "{stderr}");
        assert!(stderr.contains("json, compact"), "{stderr}");
        assert!(
            !stderr.contains("RAM-A memory config not found"),
            "{stderr}"
        );
    }
}

#[test]
fn invalid_log_source_fails_before_service_configuration() {
    for invalid in ["", "TRUE", "1", " false"] {
        let output = run_with("RAM_A_LOG_SOURCE", invalid);
        assert!(!output.status.success());
        let stderr = String::from_utf8(output.stderr).expect("UTF-8 stderr");
        assert!(stderr.contains("invalid RAM_A_LOG_SOURCE"), "{stderr}");
        assert!(stderr.contains("true, false"), "{stderr}");
        assert!(
            !stderr.contains("RAM-A memory config not found"),
            "{stderr}"
        );
    }
}
