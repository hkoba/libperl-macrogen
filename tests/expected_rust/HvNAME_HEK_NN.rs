/// HvNAME_HEK_NN - macro function
#[inline]
pub unsafe fn HvNAME_HEK_NN(hv: *const SV) -> *mut HEK {
    unsafe {
        if (*HvAUX(hv)).xhv_name_count != 0 { (*((*HvAUX(hv)).xhv_name_u).xhvnameu_names) } else { ((*HvAUX(hv)).xhv_name_u).xhvnameu_name }
    }
}
