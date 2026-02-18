/// SvOK_off - macro function
#[inline]
pub unsafe fn SvOK_off(sv: *mut SV) -> () {
    unsafe {
        { { assert!(((!((SvROK(sv)) != 0)) || (!((SvRV(sv)) != 0)))); }; { { assert!((!((isGV_with_GP(sv)) != 0))); }; { { (*sv).sv_flags &= (!((SVf_OK | SVf_IVisUV) | SVf_UTF8)); (*sv).sv_flags }; SvOOK_off(sv) } } }
    }
}
