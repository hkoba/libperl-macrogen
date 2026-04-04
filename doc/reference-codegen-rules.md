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
