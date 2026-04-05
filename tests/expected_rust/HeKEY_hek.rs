/// HeKEY_hek - macro function
#[inline]
pub unsafe fn HeKEY_hek(he: *const HE) -> *mut HEK {
    unsafe {
        (*he).hent_hek
    }
}
