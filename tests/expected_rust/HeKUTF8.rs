/// HeKUTF8 - macro function
#[inline]
pub unsafe fn HeKUTF8(he: *mut HE) -> c_int {
    unsafe { HEK_UTF8(HeKEY_hek(he)) }
}
