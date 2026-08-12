/// CvSTASH - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CvSTASH(sv: *const CV) -> *mut HV {
    unsafe {
        MUTABLE_HV(
            (*(MUTABLE_PTR((*sv).sv_any as *mut c_void) as *mut XPVCV)).xcv_stash as *mut c_void,
        )
    }
}
