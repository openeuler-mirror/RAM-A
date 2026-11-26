//! 本文件验证分析配置的业务约束。

use super::*;

#[test]
fn rejects_zero_top_k_and_non_finite_thresholds() {
    let mut options = AnalyzeConfig {
        top_k: 0,
        ..AnalyzeConfig::default()
    };
    assert!(options.validate().is_err());
    options.top_k = 1;
    options.candidate.compatibility_threshold = f64::NAN;
    assert!(options.validate().is_err());
}
