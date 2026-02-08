/// HeKWASUTF8 - macro function
#[inline]
pub unsafe fn HeKWASUTF8(he: *mut HE) -> c_int {
    unsafe { HEK_WASUTF8(HeKEY_hek(he)) }
}
