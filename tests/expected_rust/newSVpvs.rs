/// newSVpvs [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn newSVpvs(my_perl: *mut PerlInterpreter, str: &str) -> *mut SV {
    unsafe {
        Perl_newSVpvn(my_perl, str.as_ptr() as *const c_char, str.len())
    }
}
