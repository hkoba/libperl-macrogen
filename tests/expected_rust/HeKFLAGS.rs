/// HeKFLAGS - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HeKFLAGS(he: *const HE) -> c_uchar {
    unsafe {
        *(HEK_KEY(HeKEY_hek(he)) as *mut c_uchar).offset(HEK_LEN(HeKEY_hek(he)) as isize).offset(1 as isize)
    }
}
