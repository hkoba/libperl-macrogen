/// HEK_FLAGS - macro function
#[inline]
pub unsafe fn HEK_FLAGS(hek: *const HEK) -> c_uchar {
    unsafe {
        *(HEK_KEY(hek) as *mut c_uchar).offset(HEK_LEN(hek) as isize).offset(1 as isize)
    }
}
