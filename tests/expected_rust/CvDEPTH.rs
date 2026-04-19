/// CvDEPTH - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CvDEPTH(sv: *const SV) -> I32 {
    unsafe {
        *Perl_CvDEPTH(sv as *const CV)
    }
}
