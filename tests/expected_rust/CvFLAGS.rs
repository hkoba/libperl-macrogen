/// CvFLAGS - macro function
#[inline]
pub unsafe fn CvFLAGS(sv: *mut SV) -> cv_flags_t {
    unsafe { (*(MUTABLE_PTR((*sv).sv_any) as *mut XPVCV)).xcv_flags }
}
