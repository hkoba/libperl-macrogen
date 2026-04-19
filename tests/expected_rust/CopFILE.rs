/// CopFILE - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CopFILE(c: *const COP) -> *mut c_char {
    unsafe {
        (*c).cop_file
    }
}
