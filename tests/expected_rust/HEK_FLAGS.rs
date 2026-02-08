/// HEK_FLAGS - macro function
#[inline]
pub unsafe fn HEK_FLAGS(hek: *mut HEK) -> c_int {
    unsafe { (*(((HEK_KEY(hek) as *mut c_uchar) + HEK_LEN(hek)) + 1)) }
}
