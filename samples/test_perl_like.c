#define UINTMAX_C(c) c ## UL
#define nBIT_MASK(n) ((UINTMAX_C(1) << (n)) - 1)

#define RXf_PMf_STD_PMMOD_SHIFT 0
#define _RXf_PMf_SHIFT_COMPILETIME (RXf_PMf_STD_PMMOD_SHIFT+11)

#define RXf_PMf_MULTILINE      (1U << (RXf_PMf_STD_PMMOD_SHIFT+0))
#define RXf_PMf_SINGLELINE     (1U << (RXf_PMf_STD_PMMOD_SHIFT+1))
#define RXf_PMf_FOLD           (1U << (RXf_PMf_STD_PMMOD_SHIFT+2))
#define RXf_PMf_EXTENDED       (1U << (RXf_PMf_STD_PMMOD_SHIFT+3))
#define RXf_PMf_EXTENDED_MORE  (1U << (RXf_PMf_STD_PMMOD_SHIFT+4))
#define RXf_PMf_NOCAPTURE      (1U << (RXf_PMf_STD_PMMOD_SHIFT+5))
#define RXf_PMf_KEEPCOPY       (1U << (RXf_PMf_STD_PMMOD_SHIFT+6))
#define _RXf_PMf_CHARSET_SHIFT ((RXf_PMf_STD_PMMOD_SHIFT)+7)
#define RXf_PMf_CHARSET (7U << (_RXf_PMf_CHARSET_SHIFT))
#define RXf_PMf_STRICT         (1U<<(RXf_PMf_STD_PMMOD_SHIFT+10))

#define RXf_PMf_COMPILETIME \
    ( RXf_PMf_MULTILINE     \
    | RXf_PMf_SINGLELINE    \
    | RXf_PMf_FOLD          \
    | RXf_PMf_EXTENDED      \
    | RXf_PMf_EXTENDED_MORE \
    | RXf_PMf_KEEPCOPY      \
    | RXf_PMf_NOCAPTURE     \
    | RXf_PMf_CHARSET       \
    | RXf_PMf_STRICT )

#if RXf_PMf_COMPILETIME != ((nBIT_MASK(_RXf_PMf_SHIFT_COMPILETIME)) \
                        & (~(nBIT_MASK( RXf_PMf_STD_PMMOD_SHIFT))))
#error RXf_PMf_COMPILETIME is invalid
#endif

int x;
