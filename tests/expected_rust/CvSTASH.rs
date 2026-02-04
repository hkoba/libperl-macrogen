/// CvSTASH - macro function
#[inline]
pub unsafe fn CvSTASH(sv: *mut CV) -> *mut HV {
    unsafe { MUTABLE_HV((*(MUTABLE_PTR(SvANY(sv)) as *mut XPVCV)).xcv_stash) }
}
