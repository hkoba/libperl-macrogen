/// SvTYPE - macro function
#[inline]
pub unsafe fn SvTYPE(sv: *mut SV) -> svtype {
    unsafe { (((*sv).sv_flags & SVTYPEMASK) as svtype) }
}
