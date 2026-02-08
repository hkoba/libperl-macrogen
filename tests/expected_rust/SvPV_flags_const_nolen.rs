/// SvPV_flags_const_nolen [THX] - macro function
#[inline]
pub unsafe fn SvPV_flags_const_nolen(
    my_perl: *mut PerlInterpreter,
    sv: *const SV,
    flags: U32,
) -> *mut c_char {
    unsafe {
        (Perl_SvPV_helper(
            my_perl,
            sv,
            (0 as *mut c_void),
            flags,
            SvPVnormal_type_,
            Perl_sv_2pv_flags,
            0,
            SV_CONST_RETURN,
        ) as *const c_char)
    }
}
