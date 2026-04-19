/// CvSTASH - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CvSTASH(sv: *const CV) -> *mut HV {
    unsafe {
        MUTABLE_HV((*((*sv).sv_any as *mut XPVCV)).xcv_stash)
    }
}
