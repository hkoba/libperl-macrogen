# Implemented Features Reference

## GCC Extensions Supported

- `__attribute__((...))` - on functions, parameters, struct members, declarations
- `__extension__` - ignored
- `__asm__` / `asm` / `__asm` - inline assembly (skipped in parsing)
- `__typeof__` / `typeof` - typeof operator
- `__alignof__` / `__alignof` - alignof operator
- `__signed__` - signed keyword variant
- `bool` (C23/GCC) - boolean type
- `_Bool` - C99 boolean type
- `_Complex` - complex number type
- `_Float16`, `_Float32`, `_Float64`, `_Float128`, `_Float32x`, `_Float64x` - extended float types
- `__int128` - 128-bit integer type
- `_Thread_local` / `__thread` - thread-local storage (ignored)
- `({ ... })` - statement expressions
- `_Pragma(...)` - pragma operator (defined as empty macro)

## Preprocessor Features

- Object and function-like macros
- `#if`, `#ifdef`, `#ifndef`, `#elif`, `#else`, `#endif`
- `#include` and `#include_next`
- `#define` and `#undef`
- `#pragma` (ignored)
- `#error` and `#warning`
- Token pasting (`##`) and stringification (`#`)
- Variadic macros (`__VA_ARGS__`, `##__VA_ARGS__`)
- Predefined macros (`__FILE__`, `__LINE__`, etc.)
- Macro argument prescanning (C standard compliant)

## Macro Expansion Location Tracking

When errors occur in macro-expanded code, the error location points to where the macro is **used**, not where it is **defined**. This makes debugging easier.
