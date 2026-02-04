/// CopFILEAV [THX] - macro function
#[inline]
pub unsafe fn CopFILEAV(my_perl: *mut PerlInterpreter, c: *mut COP) -> *mut AV {
    unsafe {
        (if ((CopFILE(c)) != 0) {
            GvAV(gv_fetchfile(my_perl, CopFILE(c)))
        } else {
            (0 as *mut c_void)
        })
    }
}
