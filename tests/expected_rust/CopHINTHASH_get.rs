/// CopHINTHASH_get - macro function
#[inline]
pub unsafe fn CopHINTHASH_get(c: *mut COP) -> *mut COPHH {
    unsafe { ((*c).cop_hints_hash as *mut COPHH) }
}
