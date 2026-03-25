/// Perl_CvDEPTH - inline function
#[inline]
pub unsafe fn Perl_CvDEPTH(sv: *const CV) -> *mut I32 {
    unsafe {
        assert!(!sv.is_null());
        assert!(
            (((SvTYPE(sv) as u32) == (SVt_PVCV as u32))
                || ((SvTYPE(sv) as u32) == (SVt_PVFM as u32)))
        );
        return (&mut (*((*sv).sv_any as *mut XPVCV)).xcv_depth);
    }
}
