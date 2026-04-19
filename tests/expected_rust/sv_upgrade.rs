/// sv_upgrade [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn sv_upgrade(my_perl: *mut PerlInterpreter, a: *mut SV, b: svtype) -> () {
    unsafe {
        Perl_sv_upgrade(my_perl, a, b);
    }
}

