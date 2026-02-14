/// newSVpvs [THX] - macro function
#[inline]
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: &str) -> *mut SV {
    unsafe {
        Perl_newSVpvn(my_perl, str.as_ptr(), str.len())
    }
}
