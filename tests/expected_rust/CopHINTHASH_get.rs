/// CopHINTHASH_get - macro function
#[inline]
#[allow(unsafe_op_in_unsafe_fn)]
pub unsafe fn CopHINTHASH_get(c: *const COP) -> *mut COPHH {
    unsafe {
        (*c).cop_hints_hash as *mut COPHH
    }
}
