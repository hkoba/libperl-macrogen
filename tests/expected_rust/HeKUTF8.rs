/// HeKUTF8 - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HeKUTF8(he: *const HE) -> c_uchar {
    unsafe {
        HEK_UTF8(HeKEY_hek(he))
    }
}
