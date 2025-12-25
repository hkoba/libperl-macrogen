#include <bits/wordsize.h>
#if __WORDSIZE == 64
ok64
#else
not64
#endif

#define __STD_TYPE_DEF 1
#include <bits/types.h>

BEFORE
__STD_TYPE
AFTER

BEFORE2
__DEV_T_TYPE
AFTER2
