/// CopFILE - macro function
#[inline]
pub unsafe fn CopFILE(c: *const COP) -> *mut c_char {
    unsafe {
        (*c).cop_file
    }
}
