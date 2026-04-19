/// SvTYPE - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn SvTYPE(sv: *const SV) -> svtype {
    unsafe {
        std::mem::transmute::<_, svtype>(((*sv).sv_flags & SVTYPEMASK))
    }
}
