/// CopFILE - macro function
#[inline]
pub unsafe fn CopFILE(c: *mut COP) -> *mut c_char {
    unsafe { (*c).cop_file }
}
