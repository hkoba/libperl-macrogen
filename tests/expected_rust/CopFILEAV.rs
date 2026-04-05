/// CopFILEAV [THX] - macro function
#[inline]
pub unsafe fn CopFILEAV(my_perl: *mut PerlInterpreter, c: *const COP) -> *mut AV {
    unsafe {
        if !CopFILE(c).is_null() {
            GvAV(gv_fetchfile(my_perl, CopFILE(c)) as *mut GV)
        } else {
            std::ptr::null_mut()
        }
    }
}

