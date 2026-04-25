/// OP_CLASS [THX] - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn OP_CLASS(my_perl: *mut PerlInterpreter, o: *const OP) -> U32 {
    unsafe {
        if (*o).op_type() == OP_CUSTOM as u16 { Perl_custom_op_get_field(my_perl, o, XOPe_xop_class).xop_class } else { *((&raw const PL_opargs) as *const U32 as *mut U32).offset((*o).op_type() as isize) & (15 << 8) as u32 }
    }
}
