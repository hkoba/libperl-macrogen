/// SvOK_off - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn SvOK_off(sv: *mut SV) -> () {
    unsafe {
        { { assert!(!(SvROK(sv) != 0) || !SvRV(sv).is_null()); }; { { assert!(!isGV_with_GP(sv)); }; { let _ = { (*sv).sv_flags &= !(SVf_OK | SVf_IVisUV | SVf_UTF8); (*sv).sv_flags }; SvOOK_off(sv) } } };
    }
}
