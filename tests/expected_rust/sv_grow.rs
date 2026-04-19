/// sv_grow [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn sv_grow(my_perl: *mut PerlInterpreter, a: *mut SV, b: STRLEN) -> *mut c_char {
    unsafe {
        Perl_sv_grow(my_perl, a, b)
    }
}
