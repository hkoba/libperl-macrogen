/// CopLABEL [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CopLABEL(my_perl: *mut PerlInterpreter, c: *mut COP) -> *const c_char {
    unsafe {
        Perl_cop_fetch_label(my_perl, c, std::ptr::null_mut(), std::ptr::null_mut())
    }
}
