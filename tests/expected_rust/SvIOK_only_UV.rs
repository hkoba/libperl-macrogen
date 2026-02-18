/// SvIOK_only_UV - macro function
#[inline]
pub unsafe fn SvIOK_only_UV(sv: *mut SV) -> () {
    unsafe {
        { { assert!((!((isGV_with_GP(sv)) != 0))); }; { SvOK_off_exc_UV(sv); { (*sv).sv_flags |= (SVf_IOK | SVp_IOK); (*sv).sv_flags } } }
    }
}
