#define UINTMAX_C(c) c ## ULL
#define nBIT_MASK(n) ((UINTMAX_C(1) << (n)) - 1)
#if nBIT_MASK(4) != 15
#error test failed
#endif
int x;
