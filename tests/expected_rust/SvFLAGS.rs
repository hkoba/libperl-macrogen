/// SvFLAGS - macro function
#[inline]
pub unsafe fn SvFLAGS(sv: *mut SV) -> U32 {
    unsafe { (*sv).sv_flags }
}
