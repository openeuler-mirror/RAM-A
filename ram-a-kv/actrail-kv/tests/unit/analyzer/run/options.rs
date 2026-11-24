//! 本文件验证 run/options 模块的行为。

use super::*;

#[test]
fn rejects_zero_top_k_and_non_finite_thresholds() {
    let mut options = AnalysisOptions {
        top_k: 0,
        ..AnalysisOptions::default()
    };
    assert!(options.validate().is_err());
    options.top_k = 1;
    options.text_similarity_threshold = f64::NAN;
    assert!(options.validate().is_err());
}
