//! 本文件验证 main 模块的行为。

use super::*;

#[test]
fn config_defaults_to_loopback_listener() {
    let config = ReceiverConfig::default();
    let address: SocketAddr = config.listen.parse().expect("valid default address");
    assert_eq!(address, "127.0.0.1:8080".parse().expect("address"));
}
