/// CvDEPTH - macro function
#[inline]
pub unsafe fn CvDEPTH(sv: *const SV) -> I32 {
    unsafe { *Perl_CvDEPTH(sv as *const CV) }
}

