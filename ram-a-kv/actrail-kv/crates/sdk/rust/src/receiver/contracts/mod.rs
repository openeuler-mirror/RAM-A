//! 提供直接采集的类型化门面，每次成功提交均已完成后端写入。
use crate::{loader::Instance, Result, SdkConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct CaptureInput {
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub comparison: CaptureComparison,
}
#[derive(Debug, Serialize)]
pub struct CaptureComparison {
    pub endpoint_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_deployment_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kv_namespace: Option<String>,
}
#[derive(Debug, Deserialize)]
pub struct CaptureReceipt {
    pub captured_at: String,
}
pub struct Receiver {
    instance: Instance,
}
pub struct ReceiverFactory;
impl ReceiverFactory {
    /// 从受信任且遵守 actrail-kv C ABI 的动态库创建持久采集实例。
    pub fn create(config: SdkConfig) -> Result<Receiver> {
        Ok(Receiver {
            instance: Instance::create(config, "receiver")?,
        })
    }
}
impl Receiver {
    pub fn submit(&mut self, input: &CaptureInput) -> Result<CaptureReceipt> {
        let mut request = serde_json::to_value(input)?;
        request["schema_version"] = actrail_kv_sdk_contracts::SCHEMA_VERSION.into();
        let output = self.instance.call(&request)?;
        Ok(serde_json::from_str(output.data()?.get())?)
    }
    pub fn close(self) -> Result<()> {
        self.instance.close()
    }
}
