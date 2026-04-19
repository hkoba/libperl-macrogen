/// HEK_UTF8 - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HEK_UTF8(hek: *const HEK) -> c_uchar {
    unsafe {
        (((*(HEK_KEY(hek) as *mut c_uchar).offset(HEK_LEN(hek) as isize).offset(1 as isize)) as u32) & HVhek_UTF8) as u8
    }
}
