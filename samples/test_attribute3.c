#define __nonnull(args) __attribute__((__nonnull__ args))
#define __wur __attribute__((__warn_unused_result__))

extern int select (int __nfds, int *__readfds)
    __nonnull ((2)) __wur;
