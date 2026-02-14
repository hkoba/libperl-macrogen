/// CopLABEL [THX] - macro function
#[inline]
pub unsafe fn CopLABEL(my_perl: *mut PerlInterpreter, c: *mut COP) -> *mut c_char {
    unsafe {
        Perl_cop_fetch_label(my_perl, c, (0 as *mut c_void), (0 as *mut c_void))
    }
}
