/// AMG_CALLunary [THX] - macro function
#[inline]
pub unsafe fn AMG_CALLunary(my_perl: *mut PerlInterpreter, sv: *mut SV, meth: c_int) -> *mut SV {
    unsafe {
        amagic_call(my_perl, sv, (&mut (*my_perl).Isv_undef), meth, (1 | 8))
    }
}
