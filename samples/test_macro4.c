#define SHIFT_A 4
#define SHIFT_B 2
#define FLAG_1 (1 << 0)
#define FLAG_2 (1 << 1)
#define FLAG_3 (1 << 2)
#define ALL_FLAGS \
    ( FLAG_1 \
    | FLAG_2 \
    | FLAG_3 )
#define nBIT_MASK(n) (((1ULL) << (n)) - 1)
#if ALL_FLAGS != ((nBIT_MASK(SHIFT_A)) \
                & (~(nBIT_MASK(SHIFT_B))))
int should_appear;
#endif
int always_here;
