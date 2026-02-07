/// HEK_KEY - macro function
#[inline]
pub unsafe fn HEK_KEY(hek: *mut HEK) -> [c_char; 1] {
    unsafe { (*hek).hek_key }
}
