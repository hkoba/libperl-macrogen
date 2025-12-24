# Claude Code Project Guidelines

This file contains guidelines for Claude Code when working on this project.

## Project Configuration

- **Rust Edition**: 2024 (do not change this)

## Development Workflow

### Signature Approval Rule

**IMPORTANT**: Before implementing any new module or making significant changes:

1. **Present Signatures First**: Before creating a new `.rs` file, present the public API (structs, enums, function signatures) to the user for review
2. **Wait for Approval**: Do not start implementation until the user explicitly approves the signatures
3. **Apply to Changes**: When making major changes to existing modules, present signature-level changes first

### Phase-based Development

This project is developed in phases. Each phase should:
1. Start with signature approval
2. Implement the approved design
3. Test and verify before moving to the next phase

### Current Phase Structure

- **Phase 1**: Lexer foundation (completed)
- **Phase 2**: Preprocessor (completed)
- **Phase 3**: Parser + S-expression dump
  - `ast.rs` - Abstract Syntax Tree definitions
  - `parser.rs` - C language parser
  - `sexp.rs` - S-expression output
  - `main.rs` - CLI binary for S-expression dump

### Commit Guidelines

- Commit after each phase is complete
- Use descriptive commit messages explaining the changes
