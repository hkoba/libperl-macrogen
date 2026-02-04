/// Perl_CvDEPTH - inline function
#[inline]
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        assert!((sv) != 0);
        assert!(((SvTYPE(sv) == SVt_PVCV) || (SvTYPE(sv) == SVt_PVFM)));
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
