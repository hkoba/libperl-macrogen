//! Emit `PL_<name>!($my_perl)` declarative macros for each entry in
//! [`PerlvarDict`].
//!
//! Output is target-specific: only one form per entry is emitted (no
//! `#[cfg]` branches), based on the build's threading mode and the
//! variable's prefix:
//!
//! | prefix | threaded build               | non-threaded build           |
//! |--------|------------------------------|------------------------------|
//! | `I`    | `(*$my_perl).I<name>` access | `$crate::PL_<name>` global   |
//! | `G`    | `$crate::PL_<name>` global   | `$crate::PL_<name>` global   |
//!
//! The emitted macros take the interpreter pointer as a single argument
//! (`PL_main_start!(my_perl)`). Hygiene rules of `macro_rules!` make a
//! C-style no-arg form impossible — identifiers in the macro body are
//! resolved at the definition site, so the macro cannot transparently
//! capture a `my_perl` from the call site. The convention `let my_perl =
//! perl.as_ptr();` (matching Perl C `aTHX_`) is still honored at the
//! call site, just made explicit at the macro invocation.
//!
//! The non-threaded form intentionally still takes the `$my_perl`
//! argument and evaluates it once, so that the same source compiles
//! against both threading modes without modification.

use std::io::{self, Write};

use crate::perlvar_dict::{ArrayLength, PerlvarDict, PerlvarEntry, PerlvarKind};

/// Emit the PERLVAR section into `out`.
///
/// `threaded` selects which form of access is emitted for `I`-prefix
/// entries. `'G'` entries always emit the global form.
pub fn emit_perlvar_section<W: Write>(
    out: &mut W,
    dict: &PerlvarDict,
    threaded: bool,
) -> io::Result<()> {
    if dict.is_empty() {
        return Ok(());
    }

    writeln!(out)?;
    writeln!(out, "// =====================================================================")?;
    writeln!(out, "// PERLVAR Accessor Macros")?;
    writeln!(out, "// ---------------------------------------------------------------------")?;
    writeln!(out, "// Generated from PERLVAR/PERLVARI/PERLVARA/PERLVARIC observations.")?;
    writeln!(out, "// Usage: `PL_<name>!(my_perl)` where `my_perl` is `*mut PerlInterpreter`.")?;
    writeln!(out, "// (`macro_rules!` hygiene prevents a no-arg form from capturing the")?;
    writeln!(out, "// caller's `my_perl`; the explicit argument keeps the source portable")?;
    writeln!(out, "// across both threading modes.)")?;
    writeln!(out, "// =====================================================================")?;

    for entry in dict.iter() {
        emit_one(out, entry, threaded)?;
    }
    Ok(())
}

fn emit_one<W: Write>(out: &mut W, e: &PerlvarEntry, threaded: bool) -> io::Result<()> {
    let pl_name = format!("PL_{}", e.name);

    // Doc comment summarising the originating PERLVAR declaration.
    writeln!(out)?;
    writeln!(out, "/// `{pl_name}` — `PERLVAR{}({}, {}, {})`",
        kind_suffix(&e.kind), e.prefix, e.name, summary_type(e))?;
    if let Some(extra) = kind_doc_extra(&e.kind) {
        writeln!(out, "///")?;
        writeln!(out, "/// {extra}")?;
    }

    // The `'I'` prefix means per-interpreter (lives in struct interpreter).
    // The `'G'` prefix means process-global (always a `PL_<name>` static).
    let use_struct_field = threaded && e.prefix == 'I';

    writeln!(out, "#[macro_export]")?;
    writeln!(out, "macro_rules! {pl_name} {{")?;
    writeln!(out, "    ($my_perl:expr) => {{{{")?;
    if use_struct_field {
        let field = format!("I{}", e.name);
        writeln!(out, "        // type-check argument; evaluate exactly once")?;
        writeln!(out, "        let __my_perl: *mut $crate::PerlInterpreter = $my_perl;")?;
        writeln!(out, "        unsafe {{ (*__my_perl).{field} }}")?;
    } else {
        writeln!(out, "        // discard $my_perl in non-threaded build (kept for source")?;
        writeln!(out, "        // portability with the threaded form); evaluate exactly once")?;
        writeln!(out, "        let _: *mut $crate::PerlInterpreter = $my_perl;")?;
        writeln!(out, "        unsafe {{ $crate::{pl_name} }}")?;
    }
    writeln!(out, "    }}}};")?;
    writeln!(out, "}}")?;
    Ok(())
}

fn kind_suffix(k: &PerlvarKind) -> &'static str {
    match k {
        PerlvarKind::Var => "",
        PerlvarKind::Init { .. } => "I",
        PerlvarKind::Array { .. } => "A",
        PerlvarKind::Const { .. } => "IC",
    }
}

fn summary_type(e: &PerlvarEntry) -> String {
    match &e.kind {
        PerlvarKind::Array { length } => {
            let n = match length {
                ArrayLength::Literal(n) => n.to_string(),
                ArrayLength::Symbolic(s) => s.clone(),
            };
            format!("{}, {}", n, e.c_type)
        }
        _ => e.c_type.clone(),
    }
}

fn kind_doc_extra(k: &PerlvarKind) -> Option<String> {
    match k {
        PerlvarKind::Var => None,
        PerlvarKind::Init { init_expr } => Some(format!("Initial value: `{}`.", init_expr)),
        PerlvarKind::Array { length } => {
            let n = match length {
                ArrayLength::Literal(n) => n.to_string(),
                ArrayLength::Symbolic(s) => s.clone(),
            };
            Some(format!("Fixed-length array of {} elements.", n))
        }
        PerlvarKind::Const { init_expr } => {
            Some(format!("Const declaration; initial value: `{}`.", init_expr))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, prefix: char, kind: PerlvarKind, c_type: &str) -> PerlvarEntry {
        PerlvarEntry {
            name: name.to_string(),
            prefix,
            kind,
            c_type: c_type.to_string(),
        }
    }

    #[test]
    fn empty_dict_emits_nothing() {
        let dict = PerlvarDict::new();
        let mut out = Vec::new();
        emit_perlvar_section(&mut out, &dict, true).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn threaded_i_prefix_uses_struct_field() {
        let mut dict = PerlvarDict::new();
        dict.insert(entry("main_start", 'I', PerlvarKind::Var, "OP *"));
        let mut out = Vec::new();
        emit_perlvar_section(&mut out, &dict, true).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("macro_rules! PL_main_start"), "{s}");
        assert!(s.contains("($my_perl:expr)"), "{s}");
        assert!(s.contains("(*__my_perl).Imain_start"), "{s}");
        assert!(!s.contains("$crate::PL_main_start "), "{s}"); // no global form
    }

    #[test]
    fn nonthreaded_uses_global_but_takes_my_perl_arg() {
        let mut dict = PerlvarDict::new();
        dict.insert(entry("main_start", 'I', PerlvarKind::Var, "OP *"));
        let mut out = Vec::new();
        emit_perlvar_section(&mut out, &dict, false).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("($my_perl:expr)"), "{s}");
        assert!(s.contains("$crate::PL_main_start"), "{s}");
        // `__my_perl` only appears in threaded form
        assert!(!s.contains("__my_perl"), "{s}");
    }

    #[test]
    fn g_prefix_always_global_even_threaded() {
        let mut dict = PerlvarDict::new();
        dict.insert(entry("op_mutex", 'G', PerlvarKind::Var, "perl_mutex"));
        let mut out = Vec::new();
        emit_perlvar_section(&mut out, &dict, true).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("($my_perl:expr)"), "{s}");
        assert!(s.contains("$crate::PL_op_mutex"), "{s}");
        assert!(!s.contains("__my_perl"), "{s}");
    }
}
