#define SHIFT 4
#define nBIT_MASK(n) (((1ULL) << (n)) - 1)
#if nBIT_MASK(SHIFT) != 15
#error test failed
#endif
int x;
