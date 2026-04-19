/// HEK_KEY - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HEK_KEY(hek: *const HEK) -> [c_char; 1] {
    unsafe {
        (*hek).hek_key
    }
}
