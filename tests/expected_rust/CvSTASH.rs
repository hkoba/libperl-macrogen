/// CvSTASH - macro function
#[inline]
pub unsafe fn CvSTASH(sv: *mut CV) -> *mut HV {
    unsafe {
        MUTABLE_HV((*((*sv).sv_any as *mut XPVCV)).xcv_stash)
    }
}
