/// Perl_cx_topblock [THX] - inline function
#[inline]
pub unsafe fn Perl_cx_topblock(my_perl: *mut PerlInterpreter, cx: *mut PERL_CONTEXT) -> () {
    unsafe {
        assert!((cx) != 0);
        ;
        ;
        (*my_perl).Imarkstack_ptr = ((*my_perl).Imarkstack + (((*cx).cx_u).cx_blk).blku_oldmarksp);
        (*my_perl).Iscopestack_ix = (((*cx).cx_u).cx_blk).blku_oldscopesp;
        (*my_perl).Icurpm = (((*cx).cx_u).cx_blk).blku_oldpm;
        Perl_rpp_popfree_to(my_perl, ((*my_perl).Istack_base + (((*cx).cx_u).cx_blk).blku_oldsp));
    }
}
