/// SvPVx [THX] - macro function
#[inline]
pub unsafe fn SvPVx(my_perl: *mut PerlInterpreter, sv: *mut SV, len: STRLEN) -> *mut c_char {
    unsafe {
        {
            let _sv: *mut SV = sv;
            SvPV(my_perl, _sv, len)
        }
    }
}
