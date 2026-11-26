//! 验证原生工作线程隔离 panic，并在关闭返回前停止后台任务。
use actrail_kv_sdk_runtime::{Application, Config, Result, Runtime, Worker};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
static TASK_STOPPED: AtomicBool = AtomicBool::new(false);
struct Guard;
impl Drop for Guard {
    fn drop(&mut self) {
        TASK_STOPPED.store(true, Ordering::SeqCst);
    }
}
struct TestApp;
impl Application for TestApp {
    fn create(_config: Config, runtime: &Runtime) -> Result<Self> {
        let (ready, waiting) = std::sync::mpsc::sync_channel(1);
        runtime.spawn(async move {
            let _guard = Guard;
            ready.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        waiting.recv().unwrap();
        Ok(Self)
    }
    fn call(&mut self, _runtime: &Runtime, input: &[u8], _options: &[u8]) -> Result<Value> {
        assert_ne!(input, b"panic", "test operation panic");
        Ok(json!({"done":true}))
    }
}
#[test]
fn panic_poisoning_and_runtime_shutdown_are_contained() {
    let config = serde_json::to_vec(
        &json!({"schema_version":1,"base_dir":std::env::current_dir().unwrap(),"config_toml":""}),
    )
    .unwrap();
    let worker = Worker::<TestApp>::create(config).unwrap();
    assert_eq!(
        worker.call(b"ok".to_vec(), vec![]).unwrap().data["done"],
        true
    );
    assert_eq!(
        worker.call(b"panic".to_vec(), vec![]).err().unwrap().status,
        6
    );
    assert_eq!(worker.call(b"ok".to_vec(), vec![]).err().unwrap().status, 6);
    worker.close().unwrap();
    assert!(TASK_STOPPED.load(Ordering::SeqCst));
}
