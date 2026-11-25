//! 将 C ABI 的借用缓冲区与句柄转换为受控业务调用，并隔离 panic。
use crate::{
    abi::{Handle, InputBuffer, OutputBuffer, Response, SCHEMA_VERSION},
    Application, Error, Result, Worker,
};
use serde_json::json;
use std::{
    marker::PhantomData,
    panic::{catch_unwind, AssertUnwindSafe},
};

pub struct Boundary<A: Application>(PhantomData<A>);
impl<A: Application> Boundary<A> {
    /// # Safety
    /// 输入和输出地址必须在调用期间有效，输出不能与输入重叠。
    pub unsafe fn create(
        input: InputBuffer,
        handle: *mut Handle,
        output: *mut OutputBuffer,
    ) -> i32 {
        if handle.is_null() {
            return 1;
        }
        // SAFETY: 调用者保证句柄输出地址可写。
        unsafe {
            *handle = std::ptr::null_mut();
        }
        unsafe {
            Self::respond(output, || {
                let worker = Worker::<A>::create(Self::bytes(input)?.to_vec())?;
                *handle = Box::into_raw(Box::new(worker)).cast();
                Ok(json!({}).into())
            })
        }
    }
    /// # Safety
    /// 句柄须由本库创建且未销毁，缓冲区地址必须有效。
    pub unsafe fn call(
        handle: Handle,
        input: InputBuffer,
        options: InputBuffer,
        output: *mut OutputBuffer,
    ) -> i32 {
        unsafe {
            Self::respond(output, || {
                if handle.is_null() {
                    return Err(Error::input("null instance handle"));
                }
                let worker = &*handle.cast::<Worker<A>>();
                worker.call(Self::bytes(input)?.to_vec(), Self::bytes(options)?.to_vec())
            })
        }
    }
    /// # Safety
    /// 句柄须由本库创建且仅销毁一次，不得与业务操作并发。
    pub unsafe fn destroy(handle: Handle, output: *mut OutputBuffer) -> i32 {
        unsafe {
            Self::respond(output, || {
                if handle.is_null() {
                    return Err(Error::input("null instance handle"));
                }
                let worker = Box::from_raw(handle.cast::<Worker<A>>());
                (*worker).close()?;
                Ok(json!({}).into())
            })
        }
    }
    /// # Safety
    /// 两个输入缓冲区和输出地址必须在调用期间有效。
    pub unsafe fn export(
        input: InputBuffer,
        destination: InputBuffer,
        output: *mut OutputBuffer,
    ) -> i32 {
        unsafe {
            Self::respond(output, || {
                A::export(Self::bytes(input)?, Self::bytes(destination)?).map(Into::into)
            })
        }
    }
    /// # Safety
    /// 仅接收本库分配、未释放、指针和长度均未被修改的完整输出。
    pub unsafe fn free(buffer: OutputBuffer) {
        if !buffer.ptr.is_null() {
            // SAFETY: 与 respond 中 Box<[u8]> 的分配布局一致。
            unsafe {
                drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
                    buffer.ptr, buffer.len,
                )));
            }
        }
    }
    unsafe fn bytes<'a>(input: InputBuffer) -> Result<&'a [u8]> {
        if input.len == 0 {
            return Ok(&[]);
        }
        if input.ptr.is_null() || input.len > isize::MAX as usize {
            return Err(Error::input("invalid input buffer"));
        }
        // SAFETY: 外部契约保证非空指针指向至少 len 个可读字节。
        Ok(unsafe { std::slice::from_raw_parts(input.ptr, input.len) })
    }
    unsafe fn respond(
        output: *mut OutputBuffer,
        operation: impl FnOnce() -> Result<crate::worker::CallResult>,
    ) -> i32 {
        if output.is_null() {
            return 1;
        }
        // SAFETY: 调用者提供可写的独立输出槽。
        unsafe {
            *output = OutputBuffer::default();
        }
        let result = catch_unwind(AssertUnwindSafe(operation))
            .unwrap_or_else(|_| Err(Error::internal("panic at ABI boundary")));
        let (status, response) = match result {
            Ok(data) => (
                0,
                Response {
                    schema_version: SCHEMA_VERSION,
                    ok: true,
                    data: Some(data.data),
                    source: data.source,
                    error: None,
                },
            ),
            Err(error) => (
                error.status,
                Response {
                    schema_version: SCHEMA_VERSION,
                    ok: false,
                    data: None,
                    source: None,
                    error: Some(error.detail),
                },
            ),
        };
        let bytes = serde_json::to_vec(&response)
            .expect("JSON value serialization")
            .into_boxed_slice();
        let len = bytes.len();
        let ptr = Box::into_raw(bytes).cast::<u8>();
        unsafe {
            *output = OutputBuffer { ptr, len };
        }
        status
    }
}
