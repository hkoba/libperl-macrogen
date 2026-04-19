/// HvFILL [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HvFILL(my_perl: *mut PerlInterpreter, hv: *mut HV) -> STRLEN {
    unsafe {
        Perl_hv_fill(my_perl, MUTABLE_HV(hv))
    }
}
