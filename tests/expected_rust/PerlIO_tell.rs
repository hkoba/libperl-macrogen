/// PerlIO_tell [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn PerlIO_tell(my_perl: *mut PerlInterpreter, a: *mut PerlIO) -> off64_t {
    unsafe {
        Perl_PerlIO_tell(my_perl, a)
    }
}
