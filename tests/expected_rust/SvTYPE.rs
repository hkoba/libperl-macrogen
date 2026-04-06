/// SvTYPE - macro function
#[inline]
pub unsafe fn SvTYPE(sv: *const SV) -> svtype {
    unsafe {
        std::mem::transmute::<_, svtype>(((*sv).sv_flags & SVTYPEMASK))
    }
}
