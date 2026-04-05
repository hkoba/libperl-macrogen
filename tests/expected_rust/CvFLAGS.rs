/// CvFLAGS - macro function
#[inline]
pub unsafe fn CvFLAGS(sv: *const SV) -> cv_flags_t {
    unsafe {
        (*((*sv).sv_any as *mut XPVCV)).xcv_flags
    }
}
