/// Perl_CvDEPTH - inline function
#[inline]
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        assert!(!sv.is_null());
        assert!(((SvTYPE((sv as *const SV)) == SVt_PVCV) || (SvTYPE((sv as *const SV)) == SVt_PVFM)));
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
