/// PerlIO_tell [THX] - macro function
#[inline]
pub unsafe fn PerlIO_tell(my_perl: *mut PerlInterpreter, a: *mut PerlIO) -> off64_t {
    unsafe { Perl_PerlIO_tell(my_perl, a) }
}
