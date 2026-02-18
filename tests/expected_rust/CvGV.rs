/// CvGV [THX] - macro function
#[inline]
pub unsafe fn CvGV(my_perl: *mut PerlInterpreter, sv: *mut SV) -> *mut GV {
    unsafe {
        Perl_CvGV(my_perl, (sv as *mut CV))
    }
}
