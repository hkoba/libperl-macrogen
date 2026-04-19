/// HEK_HASH - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HEK_HASH(hek: *const HEK) -> U32 {
    unsafe {
        (*hek).hek_hash
    }
}
