/// CopFILEAV [THX] - macro function
#[inline]
pub unsafe fn CopFILEAV(my_perl: *mut PerlInterpreter, c: *mut COP) -> *mut AV {
    unsafe {
        (if !CopFILE(c).is_null() { GvAV(gv_fetchfile(my_perl, CopFILE(c))) } else { std::ptr::null_mut() })
    }
}
