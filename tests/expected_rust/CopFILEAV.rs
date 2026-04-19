/// CopFILEAV [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CopFILEAV(my_perl: *mut PerlInterpreter, c: *const COP) -> *mut AV {
    unsafe {
        if !CopFILE(c).is_null() { GvAV(gv_fetchfile(my_perl, CopFILE(c)) as *const SV) } else { std::ptr::null_mut() }
    }
}
