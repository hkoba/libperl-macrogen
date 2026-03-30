/// CopLABEL [THX] - macro function
#[inline]
pub unsafe fn CopLABEL(my_perl: *mut PerlInterpreter, c: *mut COP) -> *mut c_char {
    unsafe {
        (Perl_cop_fetch_label(my_perl, c, std::ptr::null_mut(), std::ptr::null_mut()) as *mut c_char)
    }
}
