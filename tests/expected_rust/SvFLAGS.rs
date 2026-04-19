/// SvFLAGS - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn SvFLAGS(sv: *const SV) -> U32 {
    unsafe {
        (*sv).sv_flags
    }
}
