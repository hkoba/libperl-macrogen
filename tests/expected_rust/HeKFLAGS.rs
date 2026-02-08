/// HeKFLAGS - macro function
#[inline]
pub unsafe fn HeKFLAGS(he: *mut HE) -> c_int {
    unsafe { (*(((HEK_KEY(HeKEY_hek(he)) as *mut c_uchar) + HEK_LEN(HeKEY_hek(he))) + 1)) }
}
