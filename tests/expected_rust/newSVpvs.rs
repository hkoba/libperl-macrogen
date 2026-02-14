/// newSVpvs [THX] - macro function
#[inline]
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: c_int) -> *mut SV {
    unsafe {
        Perl_newSVpvn(my_perl, ASSERT_IS_LITERAL(str), (std::mem::size_of_val(&str) - 1))
    }
}
