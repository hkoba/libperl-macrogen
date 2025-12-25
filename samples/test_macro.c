#define A (1)
#define B(x) ((A) & (x))
#if B(2) != 0
int x;
#endif
