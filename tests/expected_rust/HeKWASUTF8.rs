/// HeKWASUTF8 - macro function
#[inline]
pub unsafe fn HeKWASUTF8(he: *const HE) -> c_uchar {
    unsafe {
        HEK_WASUTF8(HeKEY_hek(he))
    }
}
