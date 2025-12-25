#define SHIFT_A 4
#define SHIFT_B 2
#define nBIT_MASK(n) (((1ULL) << (n)) - 1)
#define VAL ((nBIT_MASK(SHIFT_A)) & (~(nBIT_MASK(SHIFT_B))))
#if VAL != 12
#error test failed
#endif
int x;
