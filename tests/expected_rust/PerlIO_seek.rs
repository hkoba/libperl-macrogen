/// PerlIO_seek [THX] - macro function
#[inline]
pub unsafe fn PerlIO_seek(
    my_perl: *mut PerlInterpreter,
    a: *mut PerlIO,
    b: off64_t,
    c: c_int,
) -> c_int {
    unsafe { Perl_PerlIO_seek(my_perl, a, b, c) }
}
