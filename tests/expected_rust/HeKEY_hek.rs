/// HeKEY_hek - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn HeKEY_hek(he: *const HE) -> *mut HEK {
    unsafe {
        (*he).hent_hek
    }
}
