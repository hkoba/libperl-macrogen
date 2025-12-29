# Perl Apidoc Format Specification

This document describes the format used in Perl's `embed.fnc` file and `=for apidoc` comments in header files.

## Overview

The apidoc format is used to document Perl's internal API. It appears in two forms:
1. **embed.fnc file**: A standalone file containing function/macro declarations
2. **Header comments**: `=for apidoc` lines embedded in C source files

## File Format (embed.fnc)

### Comment Lines

Lines starting with `: ` (colon followed by space) are comments:

```
: This is a comment
: WARNING: Important note here
```

### Data Lines

Data lines have the format:
```
flags|return_type|name|arg1|arg2|...|argN
```

- **flags**: String of single-letter flags (described below)
- **return_type**: C return type (can be empty for void-like macros)
- **name**: Function or macro name
- **arg1...argN**: Function arguments (optional)

### Line Continuation

Lines can be continued with a backslash:
```
pr	|void	|abort_execution|NULLOK SV *msg_sv			\
				|NN const char * const name
```

### Whitespace

Leading and trailing whitespace is ignored in each component.

## Argument Prefixes

Pointer and numeric arguments can have special prefixes:

| Prefix | Meaning |
|--------|---------|
| `NN` | Not Null - pointer must not be NULL |
| `NULLOK` | Nullable - pointer may be NULL |
| `NZ` | Non-Zero - numeric argument must not be zero |

Example:
```
Adp	|SV *	|amagic_call	|NN SV *left				\
				|NN SV *right				\
				|int method				\
				|int dir
```

## Flag Reference

### Visibility and Export Flags

| Flag | Description |
|------|-------------|
| `A` | **API function** - Both long and short names are accessible everywhere. Part of the public API. Documentation goes in perlapi.pod. |
| `C` | **Core use only** - Accessible everywhere but not for public use. Documentation goes in perlintern.pod. |
| `E` | **Extension visible** - Visible to extensions compiled with PERL_EXT symbol. |
| `X` | **Explicitly exported** - Added to export list but short name macro suppressed outside core. |
| `e` | **Not exported** - Suppress entry in the list of symbols. |

### Function Type Flags

| Flag | Description |
|------|-------------|
| `p` | **Perl_ prefix** - Function has `Perl_` prefix in source code. |
| `S` | **Static function** - Function has `S_` prefix and is static. |
| `s` | **Static with Perl_ prefix** - Static function but with `Perl_` prefix. |
| `i` | **Inline static** - Compiler is requested to inline. Declared as `PERL_STATIC_INLINE`. |
| `I` | **Force inline** - Same as `i` but adds `__attribute__((always_inline))`. |
| `m` | **Macro only** - Implemented as a macro, no function exists. |
| `M` | **Custom macro** - Implementation furnishes its own macro instead of auto-generated one. |
| `T` | **No thread context** - Has no implicit interpreter/thread context argument. |

### Documentation Flags

| Flag | Description |
|------|-------------|
| `d` | **Documented** - Function has documentation somewhere in source. |
| `h` | **Hide docs** - Hide documentation from perlapi/perlintern, use link instead. |
| `U` | **No usage example** - autodoc.pl will not output a usage example. |

### Attribute Flags

| Flag | Description |
|------|-------------|
| `a` | **Allocates memory** - Like malloc/calloc. Implies `R`. |
| `P` | **Pure function** - No effects except return value. Implies `R`. |
| `R` | **Return value required** - Return value must not be ignored. |
| `r` | **No return** - Function never returns. |
| `D` | **Deprecated** - Function is deprecated. |
| `b` | **Binary compatibility** - Kept for legacy applications. |

### Format and Argument Flags

| Flag | Description |
|------|-------------|
| `f` | **Format string** - Function takes printf/strftime format string. |
| `F` | **Varargs no format** - Has `...` but don't assume it's a format. |
| `n` | **No arguments** - Macro is used without parentheses. |
| `u` | **Unorthodox** - Return value or parameters are non-standard. |
| `v` | **VA_ARGS guard** - Guard macro with `!MULTIPLICITY || PERL_CORE`. |
| `W` | **Depth argument** - Add comma_pDEPTH argument under DEBUGGING. |

### Miscellaneous Flags

| Flag | Description |
|------|-------------|
| `G` | **Suppress ARGS_ASSERT** - Don't generate empty `PERL_ARGS_ASSERT_foo` macro. |
| `N` | **Non-standard name** - Name contains non-word characters. |
| `O` | **Old compatibility** - Has a `perl_` compatibility macro. |
| `o` | **No short macro** - Suppress `#define foo Perl_foo`. |
| `x` | **Experimental** - May change in future versions. |
| `y` | **Typedef** - Element names a type rather than function/macro. |
| `;` | **Semicolon** - Add terminating semicolon to usage example. |
| `#` | **Preprocessor** - This is a `#define`/`#undef` symbol. |
| `?` | **Unknown** - Used internally by Devel::PPPort. |

## Apidoc in Source Comments

In C header files, apidoc entries appear as:

```c
/*
=for apidoc name

Documentation text here...

=cut
*/
```

Or with full specification (for macros not in embed.fnc):

```c
=for apidoc flags|return_type|name|arg1|arg2|...|argN
```

### Apidoc Item

Related functions can share documentation using `=for apidoc_item`:

```c
=for apidoc    Am|char*      |SvPV       |SV* sv|STRLEN len
=for apidoc_item |const char*|SvPV_const |SV* sv|STRLEN len
=for apidoc_item |char*      |SvPV_nolen |SV* sv

Documentation for all three macros...
```

### Apidoc Section

```c
=for apidoc_section Section Name
```

## Special Argument Conventions

For macros with non-C-parameter arguments:

| Convention | Meaning |
|------------|---------|
| `type` | Argument names a type |
| `cast` | Argument names a type for casting |
| `SP` | The stack pointer |
| `block` | A C brace-enclosed block |
| `number` | A numeric constant |
| `token` | A generic preprocessor token |
| `"string"` | A literal double-quoted string |

Example:
```c
=for apidoc Am|void|Newxc|void* ptr|int nitems|type|cast
```

## Examples

### Simple Function
```
Adp	|SV *	|av_pop 	|NN AV *av
```
- Flags: `A` (API), `d` (documented), `p` (Perl_ prefix)
- Returns: `SV *`
- Name: `av_pop`
- Args: `NN AV *av` (non-null AV pointer)

### Static Inline Function
```
ARdip	|Size_t |av_count	|NN AV *av
```
- Flags: `A` (API), `R` (return required), `d` (documented), `i` (inline), `p` (Perl_ prefix)
- Returns: `Size_t`
- Name: `av_count`
- Args: `NN AV *av`

### Macro
```
ARdm	|SSize_t|av_tindex	|NN AV *av
```
- Flags: `A` (API), `R` (return required), `d` (documented), `m` (macro)
- Returns: `SSize_t`
- Name: `av_tindex`
- Args: `NN AV *av`

### Multi-line Entry
```
Adp	|OP *	|apply_builtin_cv_attributes				\
				|NN CV *cv				\
				|NULLOK OP *attrlist
```
- Returns: `OP *`
- Name: `apply_builtin_cv_attributes`
- Args: `NN CV *cv`, `NULLOK OP *attrlist`

## Usage for Type Inference

This format provides valuable type information for:
1. **Return types**: The `return_type` field gives the C return type
2. **Parameter types**: Arguments include both the type and name
3. **Nullability**: `NN` and `NULLOK` prefixes indicate pointer nullability
4. **Function characteristics**: Flags like `m` (macro) or `i` (inline) help categorize

When parsing for type inference, focus on:
- The return type (second field)
- Parameter types (extracted from argN fields)
- The `m` flag to distinguish macros from functions
