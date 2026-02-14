/// OP_CLASS [THX] - macro function
#[inline]
pub unsafe fn OP_CLASS(my_perl: *mut PerlInterpreter, o: *mut OP) -> U32 {
    unsafe {
        (if ((*o).op_type == OP_CUSTOM) { (Perl_custom_op_get_field(my_perl, o, XOPe_xop_class)).xop_class } else { ((*PL_opargs.offset((*o).op_type as isize)) & (15 << 8)) })
    }
}
