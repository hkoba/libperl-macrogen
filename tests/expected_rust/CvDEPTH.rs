/// CvDEPTH - macro function
#[inline]
pub unsafe fn CvDEPTH(sv: *mut SV) -> I32 {
    unsafe {
        (*Perl_CvDEPTH((sv as *const CV)))
    }
}
