//! 本文件验证 config 模块的行为。

use super::*;

#[test]
fn json_round_trip_preserves_defaults() {
    let config = ReceiverConfig::default();
    let json = config.to_json_pretty().expect("serialize");
    let parsed = ReceiverConfig::from_json_str(&json).expect("parse");
    assert_eq!(parsed, config);
}
