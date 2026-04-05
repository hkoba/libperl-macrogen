/// SvFLAGS - macro function
#[inline]
pub unsafe fn SvFLAGS(sv: *const SV) -> U32 {
    unsafe {
        (*sv).sv_flags
    }
}
