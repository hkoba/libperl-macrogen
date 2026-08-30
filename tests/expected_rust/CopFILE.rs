/// CopFILE - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CopFILE(c: *const COP) -> *const c_char {
    unsafe {
        (*c).cop_file as *const c_char
    }
}
