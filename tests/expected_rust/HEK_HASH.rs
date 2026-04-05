/// HEK_HASH - macro function
#[inline]
pub unsafe fn HEK_HASH(hek: *const HEK) -> U32 {
    unsafe {
        (*hek).hek_hash
    }
}
