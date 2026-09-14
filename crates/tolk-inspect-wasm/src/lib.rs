use serde::Serialize;
use wasm_bindgen::prelude::*;

#[wasm_bindgen(js_name = inspectProjectSnapshot)]
pub fn inspect_project_snapshot(input: JsValue) -> Result<JsValue, JsValue> {
    install_tree_sitter_allocator();
    let input = serde_wasm_bindgen::from_value(input).map_err(js_error)?;
    let snapshot = tolk_inspect_core::inspect(input).map_err(js_error)?;
    snapshot
        .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
        .map_err(js_error)
}

#[wasm_bindgen(js_name = versionInfo)]
pub fn version_info() -> Result<JsValue, JsValue> {
    tolk_inspect_core::VersionInfo {
        package_version: env!("CARGO_PKG_VERSION"),
        acton_revision: tolk_inspect_core::ACTON_REVISION,
        tolk_version: tolk_inspect_core::TOLK_VERSION,
    }
    .serialize(&serde_wasm_bindgen::Serializer::json_compatible())
    .map_err(js_error)
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

#[cfg(target_arch = "wasm32")]
fn install_tree_sitter_allocator() {
    tree_sitter_allocator::install();
}

#[cfg(not(target_arch = "wasm32"))]
const fn install_tree_sitter_allocator() {}

#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod tree_sitter_allocator {
    use std::alloc::{Layout, alloc, alloc_zeroed, dealloc};
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::ptr;
    use std::sync::Once;

    const ALIGN: usize = 16;
    const HEADER_SIZE: usize = size_of::<Header>();
    static INSTALL: Once = Once::new();

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Header {
        layout_size: usize,
        offset: usize,
    }

    pub(crate) fn install() {
        INSTALL.call_once(|| unsafe {
            tree_sitter::set_allocator(Some(malloc), Some(calloc), Some(realloc), Some(free));
        });
    }

    unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        unsafe { allocate(size, false) }
    }
    unsafe extern "C" fn calloc(count: usize, size: usize) -> *mut c_void {
        count
            .checked_mul(size)
            .map_or(ptr::null_mut(), |size| unsafe { allocate(size, true) })
    }
    unsafe extern "C" fn realloc(pointer: *mut c_void, size: usize) -> *mut c_void {
        if pointer.is_null() {
            return unsafe { allocate(size, false) };
        }
        if size == 0 {
            unsafe { free(pointer) };
            return ptr::null_mut();
        }
        let header = unsafe { read_header(pointer) };
        let replacement = unsafe { allocate(size, false) };
        if replacement.is_null() {
            return ptr::null_mut();
        }
        unsafe {
            ptr::copy_nonoverlapping(
                pointer.cast::<u8>(),
                replacement.cast::<u8>(),
                header.requested_size().min(size),
            );
            free(pointer);
        }
        replacement
    }
    unsafe extern "C" fn free(pointer: *mut c_void) {
        if pointer.is_null() {
            return;
        }
        let header = unsafe { read_header(pointer) };
        let base = unsafe { pointer.cast::<u8>().sub(header.offset) };
        let layout = unsafe { Layout::from_size_align_unchecked(header.layout_size, ALIGN) };
        unsafe { dealloc(base, layout) };
    }
    unsafe fn allocate(size: usize, zeroed: bool) -> *mut c_void {
        if size == 0 {
            return ptr::null_mut();
        }
        let Some(layout_size) = size
            .checked_add(HEADER_SIZE)
            .and_then(|size| size.checked_add(ALIGN - 1))
        else {
            return ptr::null_mut();
        };
        let Ok(layout) = Layout::from_size_align(layout_size, ALIGN) else {
            return ptr::null_mut();
        };
        let base = if zeroed {
            unsafe { alloc_zeroed(layout) }
        } else {
            unsafe { alloc(layout) }
        };
        if base.is_null() {
            return ptr::null_mut();
        }
        let address = (base as usize + HEADER_SIZE + ALIGN - 1) & !(ALIGN - 1);
        let user = address as *mut u8;
        unsafe {
            user.sub(HEADER_SIZE).cast::<Header>().write(Header {
                layout_size,
                offset: address - base as usize,
            });
        }
        user.cast()
    }
    unsafe fn read_header(pointer: *mut c_void) -> Header {
        unsafe {
            pointer
                .cast::<u8>()
                .sub(HEADER_SIZE)
                .cast::<Header>()
                .read()
        }
    }
    impl Header {
        const fn requested_size(self) -> usize {
            self.layout_size - HEADER_SIZE - (ALIGN - 1)
        }
    }
}
