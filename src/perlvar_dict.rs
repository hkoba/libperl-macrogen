//! `PerlvarDict` — collected PERLVAR/PERLVARI/PERLVARA/PERLVARIC entries.
//!
//! These entries are observed during Phase 1 (preprocessing) by registering
//! a [`PerlvarCollector`] as a [`crate::MacroCalledCallback`] for each of the
//! PERLVAR macro variants. The C preprocessor then routes every PERLVAR
//! invocation it encounters in the include tree (`wrapper.h` → `perl.h` →
//! `intrpvar.h` / `perlvars.h`) to the collector, so cpp guards (`#ifdef`,
//! `#if defined(...)`) are evaluated correctly without our parser having to
//! reimplement them.
//!
//! Phase 3 reads the dict from `InferResult` and emits one `PL_<name>!()`
//! declarative macro per entry, formatted for the target Perl's threading
//! mode (no `#[cfg]` in the output — see `docs/plan/README.md` §3.2 in the
//! consumer project).

use std::any::Any;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use crate::preprocessor::MacroCalledCallback;
use crate::token::Token;
use crate::StringInterner;

/// One PERLVAR-family entry. Sized intentionally small — the C type and
/// initializer are kept as raw text because the emitter passes them through
/// without manipulation. (See CLAUDE.md "Structure-First Type Handling":
/// PERLVAR types are an explicit boundary case where the *consumer* of the
/// emitted macro is the Rust compiler itself, not our type analysis.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PerlvarEntry {
    /// Bare variable name, without `PL_` prefix (e.g. `"stack_sp"`).
    pub name: String,

    /// `'I'` for per-interpreter (intrpvar.h) variables, `'G'` for
    /// process-global (perlvars.h) variables. The character is taken
    /// verbatim from the macro's first argument.
    pub prefix: char,

    /// Variant of the PERLVAR macro that introduced this entry.
    pub kind: PerlvarKind,

    /// The C type as a single string (e.g. `"SV **"`, `"HV *"`,
    /// `"perl_mutex"`). Whitespace inside is normalized to single spaces.
    pub c_type: String,
}

/// Variant of the PERLVAR macro that produced an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PerlvarKind {
    /// `PERLVAR(prefix, name, type)` — plain declaration.
    Var,
    /// `PERLVARI(prefix, name, type, init)` — declaration with initializer.
    /// The init expression is preserved verbatim.
    Init { init_expr: String },
    /// `PERLVARA(prefix, name, n, type)` — fixed-length array.
    Array { length: ArrayLength },
    /// `PERLVARIC(prefix, name, type, init)` — const declaration with
    /// initializer. (Currently unused by upstream Perl but supported for
    /// completeness.)
    Const { init_expr: String },
}

/// Length argument of a PERLVARA. Usually a numeric literal, but the
/// preprocessor will hand us `SVt_LAST`-style `#define`d symbols too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArrayLength {
    Literal(usize),
    Symbolic(String),
}

/// Collection of PERLVAR entries observed during preprocessing.
///
/// Iteration order is alphabetical by name (via `BTreeMap`) so that the
/// emitted Rust output is reproducible across builds.
#[derive(Debug, Default, Clone)]
pub struct PerlvarDict {
    entries: BTreeMap<String, PerlvarEntry>,
}

impl PerlvarDict {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, entry: PerlvarEntry) {
        self.entries.insert(entry.name.clone(), entry);
    }

    pub fn get(&self, name: &str) -> Option<&PerlvarEntry> {
        self.entries.get(name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PerlvarEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ─────────────────────────────────────────────────────────────────
// Collector callback (registered with the preprocessor)
// ─────────────────────────────────────────────────────────────────

/// Which PERLVAR variant a single collector instance is watching.
/// One collector is registered per macro name (PERLVAR/PERLVARI/PERLVARA/
/// PERLVARIC); they all share the same `Rc<RefCell<PerlvarDict>>`.
#[derive(Debug, Clone, Copy)]
enum CollectorKind {
    Var,
    Init,
    Array,
    Const,
}

/// Callback that records PERLVAR invocations into a shared dict.
///
/// Construct a set of four collectors (one per PERLVAR variant) sharing a
/// dict via [`PerlvarCollector::new_set`], register each on the preprocessor
/// with [`crate::Preprocessor::set_macro_called_callback`], and read the
/// dict back from the shared `Rc` after preprocessing completes.
pub struct PerlvarCollector {
    dict: Rc<RefCell<PerlvarDict>>,
    kind: CollectorKind,
}

impl PerlvarCollector {
    /// Returns the shared dict and a 4-tuple of collectors, one per macro
    /// variant. Register each collector under its corresponding macro name.
    pub fn new_set() -> (
        Rc<RefCell<PerlvarDict>>,
        PerlvarCollector,
        PerlvarCollector,
        PerlvarCollector,
        PerlvarCollector,
    ) {
        let dict = Rc::new(RefCell::new(PerlvarDict::new()));
        (
            dict.clone(),
            PerlvarCollector { dict: dict.clone(), kind: CollectorKind::Var },
            PerlvarCollector { dict: dict.clone(), kind: CollectorKind::Init },
            PerlvarCollector { dict: dict.clone(), kind: CollectorKind::Array },
            PerlvarCollector { dict, kind: CollectorKind::Const },
        )
    }
}

impl MacroCalledCallback for PerlvarCollector {
    fn on_macro_called(&mut self, args: Option<&[Vec<Token>]>, interner: &StringInterner) {
        let Some(args) = args else { return };
        // All four PERLVAR variants take at least 3 args.
        if args.len() < 3 {
            return;
        }
        let prefix = parse_prefix(&args[0], interner);
        let name = arg_to_string(&args[1], interner);
        let entry = match self.kind {
            CollectorKind::Var => {
                // PERLVAR(prefix, name, type)
                let c_type = arg_to_string(&args[2], interner);
                PerlvarEntry {
                    name,
                    prefix,
                    kind: PerlvarKind::Var,
                    c_type,
                }
            }
            CollectorKind::Init => {
                // PERLVARI(prefix, name, type, init)
                if args.len() < 4 {
                    return;
                }
                let c_type = arg_to_string(&args[2], interner);
                let init_expr = arg_to_string(&args[3], interner);
                PerlvarEntry {
                    name,
                    prefix,
                    kind: PerlvarKind::Init { init_expr },
                    c_type,
                }
            }
            CollectorKind::Array => {
                // PERLVARA(prefix, name, n, type)
                if args.len() < 4 {
                    return;
                }
                let length = parse_array_length(&args[2], interner);
                let c_type = arg_to_string(&args[3], interner);
                PerlvarEntry {
                    name,
                    prefix,
                    kind: PerlvarKind::Array { length },
                    c_type,
                }
            }
            CollectorKind::Const => {
                // PERLVARIC(prefix, name, type, init)
                if args.len() < 4 {
                    return;
                }
                let c_type = arg_to_string(&args[2], interner);
                let init_expr = arg_to_string(&args[3], interner);
                PerlvarEntry {
                    name,
                    prefix,
                    kind: PerlvarKind::Const { init_expr },
                    c_type,
                }
            }
        };
        self.dict.borrow_mut().insert(entry);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

// ─────────────────────────────────────────────────────────────────
// Helpers
// ─────────────────────────────────────────────────────────────────

/// Convert a comma-separated arg's token list back into a single
/// whitespace-normalized string. We deliberately lose layout because the
/// emitter doesn't need it; doc comments embed this verbatim.
fn arg_to_string(tokens: &[Token], interner: &StringInterner) -> String {
    let mut out = String::new();
    let mut prev_was_word = false;
    for t in tokens {
        let s = t.kind.format(interner);
        if s.is_empty() {
            continue;
        }
        let starts_word = s
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
        if prev_was_word && starts_word {
            out.push(' ');
        }
        out.push_str(&s);
        prev_was_word = s
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_');
    }
    out.trim().to_string()
}

fn parse_prefix(tokens: &[Token], interner: &StringInterner) -> char {
    let s = arg_to_string(tokens, interner);
    s.chars().next().unwrap_or('?')
}

fn parse_array_length(tokens: &[Token], interner: &StringInterner) -> ArrayLength {
    let s = arg_to_string(tokens, interner);
    if let Ok(n) = s.parse::<usize>() {
        ArrayLength::Literal(n)
    } else {
        ArrayLength::Symbolic(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dict_iteration_is_sorted() {
        let mut d = PerlvarDict::new();
        for n in ["zebra", "alpha", "mango"] {
            d.insert(PerlvarEntry {
                name: n.to_string(),
                prefix: 'I',
                kind: PerlvarKind::Var,
                c_type: "int".to_string(),
            });
        }
        let names: Vec<&str> = d.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "mango", "zebra"]);
    }

    #[test]
    fn array_length_classification() {
        assert_eq!(parse_array_length_str("4"), ArrayLength::Literal(4));
        assert_eq!(
            parse_array_length_str("SVt_LAST"),
            ArrayLength::Symbolic("SVt_LAST".to_string())
        );
    }

    fn parse_array_length_str(s: &str) -> ArrayLength {
        if let Ok(n) = s.parse::<usize>() {
            ArrayLength::Literal(n)
        } else {
            ArrayLength::Symbolic(s.to_string())
        }
    }
}
