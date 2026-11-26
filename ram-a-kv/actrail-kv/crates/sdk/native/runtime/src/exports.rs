//! 生成统一的版本、构造、析构和缓冲区释放符号，业务符号由各库声明。
#[macro_export]
macro_rules! export_lifecycle {
    ($app:ty, $major:ident, $minor:ident, $create:ident, $destroy:ident, $free:ident) => {
        #[no_mangle]
        pub extern "C" fn $major() -> u32 {
            $crate::abi::ABI_MAJOR
        }
        #[no_mangle]
        pub extern "C" fn $minor() -> u32 {
            $crate::abi::ABI_MINOR
        }
        /// # Safety
        /// 调用者须遵守 SDK 头文件中的缓冲区与输出地址约定。
        #[no_mangle]
        pub unsafe extern "C" fn $create(
            input: $crate::abi::InputBuffer,
            handle: *mut $crate::abi::Handle,
            output: *mut $crate::abi::OutputBuffer,
        ) -> i32 {
            unsafe { $crate::Boundary::<$app>::create(input, handle, output) }
        }
        /// # Safety
        /// 句柄由本库创建，只能在业务调用完成后销毁一次。
        #[no_mangle]
        pub unsafe extern "C" fn $destroy(
            handle: $crate::abi::Handle,
            output: *mut $crate::abi::OutputBuffer,
        ) -> i32 {
            unsafe { $crate::Boundary::<$app>::destroy(handle, output) }
        }
        /// # Safety
        /// 缓冲区由本库分配，指针和长度未修改且尚未释放。
        #[no_mangle]
        pub unsafe extern "C" fn $free(buffer: $crate::abi::OutputBuffer) {
            unsafe { $crate::Boundary::<$app>::free(buffer) }
        }
    };
}
