/// HEK_UTF8 - macro function
#[inline]
pub unsafe fn HEK_UTF8(hek: *mut HEK) -> c_int {
    unsafe { ((*(((HEK_KEY(hek) as *mut c_uchar) + HEK_LEN(hek)) + 1)) & HVhek_UTF8) }
}
