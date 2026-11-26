//! 定义宿主和原生库共享的 C 布局、版本与 JSON 响应契约。
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::ffi::c_void;

pub const ABI_MAJOR: u32 = 1;
pub const ABI_MINOR: u32 = 0;
pub const SCHEMA_VERSION: u32 = 1;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct InputBuffer {
    pub ptr: *const u8,
    pub len: usize,
}
impl InputBuffer {
    pub fn new(bytes: &[u8]) -> Self {
        Self {
            ptr: bytes.as_ptr(),
            len: bytes.len(),
        }
    }
}
#[repr(C)]
pub struct OutputBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}
impl Default for OutputBuffer {
    fn default() -> Self {
        Self {
            ptr: std::ptr::null_mut(),
            len: 0,
        }
    }
}

pub type Handle = *mut c_void;
pub type VersionFn = unsafe extern "C" fn() -> u32;
pub type CreateFn = unsafe extern "C" fn(InputBuffer, *mut Handle, *mut OutputBuffer) -> i32;
pub type CallFn = unsafe extern "C" fn(Handle, InputBuffer, *mut OutputBuffer) -> i32;
pub type RenderFn =
    unsafe extern "C" fn(Handle, InputBuffer, InputBuffer, *mut OutputBuffer) -> i32;
pub type DestroyFn = unsafe extern "C" fn(Handle, *mut OutputBuffer) -> i32;
pub type FreeFn = unsafe extern "C" fn(OutputBuffer);
pub type ExportFn = unsafe extern "C" fn(InputBuffer, InputBuffer, *mut OutputBuffer) -> i32;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub schema_version: u32,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<Value>,
}
