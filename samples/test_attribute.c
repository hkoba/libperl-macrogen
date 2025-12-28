#define __nonnull(args) __attribute__((__nonnull__ args))

int foo(int *p) __nonnull((1));
