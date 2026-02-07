/// HeKEY_hek - macro function
#[inline]
pub unsafe fn HeKEY_hek(he: *mut HE) -> *mut HEK {
    unsafe { (*he).hent_hek }
}
