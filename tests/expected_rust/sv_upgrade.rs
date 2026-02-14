/// sv_upgrade [THX] - macro function
#[inline]
pub unsafe fn sv_upgrade(my_perl: *mut PerlInterpreter, a: *mut SV, b: svtype) -> () {
    unsafe {
        Perl_sv_upgrade(my_perl, a, b)
    }
}
