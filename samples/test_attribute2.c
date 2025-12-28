#define __nonnull(args) __attribute__((__nonnull__ args))
#define __THROW

extern int select (int __nfds, int *__readfds) __nonnull ((2)) __THROW;
