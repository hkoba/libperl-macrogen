# Code Generation Rules Reference

## Consistency Principle

**Code generation must behave consistently for both C inline functions and C macro functions.**
The same consistent behavior must also apply to function call arguments.

## Macro Handling Rules

Rules for handling macros in generated code:

| Macro Type | Condition | Action |
|------------|-----------|--------|
| Object macro (constant) | Corresponding constant exists in Rust | Output as Rust constant |
| Object macro (constant) | No Rust counterpart | Expand inline |
| Function macro | **Not registered** in special dictionaries | Preserve as function call |
| Function macro | Registered in `ExplicitExpandSymbols` | Expand (e.g., `SvANY`, `SvFLAGS`) |
| assert family | `NoExpandSymbols` / `wrapped_macros` | Ignore `DEBUGGING` state, process arguments and generate as `assert!` |
| Function macro | apidoc return type is `pair` (e.g., `STR_WITH_LEN`) | **Do not generate a fn** (`[CODEGEN_SUPPRESSED]`, Phase 2 Step 4.45). Comma expression cannot be a single-value Rust fn. Callers normally token-expand it away; a caller with a surviving AST call is downgraded via `called_functions` check, NOT via `used_by` propagation (GH #14, doc/plan/skip-pair-return-type-macros.md Step 1) |

### Key Implication

- **Default behavior is "preserve function macros"**
- Expansion only for explicitly specified macros
- This rule should apply to both **Preprocessor** (for inline functions) and **TokenExpander** (for macros)

### Current Implementation Gap

| Processing Engine | Target | Default for Function Macros |
|-------------------|--------|----------------------------|
| `TokenExpander` | Macros | Preserve |
| `Preprocessor` | Inline functions | **Expand** ← needs fix |

The `Preprocessor`'s `wrapped_macros` argument expansion needs to behave equivalently to `TokenExpander`.
