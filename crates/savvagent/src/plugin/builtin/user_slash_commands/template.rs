//! Single-pass templating expansion for command bodies.
//!
//! Task 7: `$ARGUMENTS` and `$N` substitution.
//! Task 8 adds `@<path>` file inclusion.
//! Task 9 adds `!<cmd>` shell substitution.
//! Task 10 wires them together in `expand_all` with trust-level gating.

#![allow(dead_code)] // consumed by Task 10 (expand_all) and Task 19 (handle_slash)

/// Outcome of expanding a command body.
#[derive(Debug, Clone, Default)]
pub struct Expanded {
    /// The rendered prompt text.
    pub text: String,
    /// Non-fatal warnings emitted during expansion.
    pub warnings: Vec<String>,
}

/// Substitute `$ARGUMENTS` and `$1`/`$2`/… in `body`.
///
/// `$ARGUMENTS` becomes the raw argument string (`args.join(" ")`).
/// `$N` substitutions reference whitespace-split positional args;
/// out-of-range positions expand to the empty string. Supports `$1`
/// through `$9` only; multi-digit positions (`$10`+) are out of scope
/// for v1.
pub fn expand_args(body: &str, args: &[String]) -> String {
    let raw = args.join(" ");
    let mut out = body.replace("$ARGUMENTS", &raw);
    // Replace $1..$9 in argument order; values not present become empty.
    for (idx, a) in args.iter().take(9).enumerate() {
        out = out.replace(&format!("${}", idx + 1), a);
    }
    // Blank out any remaining $N referring to out-of-range positions.
    for n in (args.len() + 1)..=9 {
        out = out.replace(&format!("${n}"), "");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn arguments_token() {
        assert_eq!(expand_args("hello $ARGUMENTS", &s(&["a", "b"])), "hello a b");
    }

    #[test]
    fn positional() {
        assert_eq!(
            expand_args("first=$1 second=$2", &s(&["foo", "bar"])),
            "first=foo second=bar"
        );
    }

    #[test]
    fn out_of_range_is_empty() {
        assert_eq!(expand_args("[$3]", &s(&["foo"])), "[]");
    }

    #[test]
    fn no_args_is_identity_modulo_blanking_positionals() {
        assert_eq!(expand_args("plain body", &[]), "plain body");
        assert_eq!(expand_args("hi $1", &[]), "hi ");
    }
}
