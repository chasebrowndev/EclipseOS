// SPDX-License-Identifier: AGPL-3.0-only
//! Argument parsing. Hand-rolled and pure so it is testable without a
//! terminal. A value or passphrase is never an argument: argv is readable
//! from `/proc/<pid>/cmdline` by every process of the same user, which
//! includes a sandboxed agent's neighbours.

use ec_brokerd::bind::Binding;
use ec_brokerd::record::name_ok;

#[derive(Debug, PartialEq, Eq)]
pub enum Cmd {
    Help,
    Status,
    Init,
    Unlock,
    Lock,
    List,
    Add {
        name: String,
        bindings: Vec<Binding>,
        stdin: bool,
    },
    Rotate {
        name: String,
        stdin: bool,
    },
    Revoke {
        name: String,
    },
}

pub const HELP: &str = "\
ec-secret: the owner's CLI for the EclipseOS secrets broker (ec-brokerd, S-08)

USAGE
  ec-secret status                     initialised? locked or unlocked? (never values)
  ec-secret init                       first-time setup: new passphrase, asked twice
  ec-secret unlock                     unlock the store with its passphrase
  ec-secret lock                       lock the store now
  ec-secret add <name> --bind <b> [--bind <b> ...] [--stdin]
                                       store a new secret; the value is read from the
                                       terminal with echo off
  ec-secret rotate <name> [--stdin]    replace a secret's value
  ec-secret revoke <name>              delete a secret
  ec-secret list                       names, bindings, rotation counter (never values)

BINDINGS
  host:api.anthropic.com               a host (port defaults to 443)
  url:https://example.com/path         a URL prefix
  app:<app-id>                         a desktop application

  A secret is only ever released to a destination one of its bindings names.
  `add` stores a bearer secret for the proxy-header injection mode, which is
  what the egress proxy and the inference router use.

VALUES
  Never given on the command line or in the environment. `add` and `rotate`
  read the value from the terminal with echo off. For scripts, --stdin reads
  one line from a piped (non-terminal) stdin; it is never echoed either.

FIRST RUN, for the inference router (ec-inferenced, ADR 0076)
  ec-secret init
  ec-secret unlock
  ec-secret add anthropic-api-key --bind host:api.anthropic.com

EXIT CODES
  0 ok   1 refused by brokerd   2 usage   3 brokerd unreachable
";

fn want_name(n: Option<&String>) -> Result<String, String> {
    let n = n.ok_or("a secret name is required")?;
    if !name_ok(n) {
        return Err("a secret name is 1 to 128 characters of A-Z a-z 0-9 . _ -".into());
    }
    Ok(n.clone())
}

pub fn parse(args: &[String]) -> Result<Cmd, String> {
    let Some(sub) = args.first() else {
        return Err("a subcommand is required".into());
    };
    if args.iter().any(|a| a == "--help" || a == "-h") || sub == "help" {
        return Ok(Cmd::Help);
    }
    let rest = &args[1..];
    let no_extra = |cmd: Cmd| {
        if rest.is_empty() {
            Ok(cmd)
        } else {
            Err(format!("`{sub}` takes no arguments"))
        }
    };
    match sub.as_str() {
        "status" => no_extra(Cmd::Status),
        "init" => no_extra(Cmd::Init),
        "unlock" => no_extra(Cmd::Unlock),
        "lock" => no_extra(Cmd::Lock),
        "list" => no_extra(Cmd::List),
        "revoke" => {
            if rest.len() > 1 {
                return Err("`revoke` takes exactly one name".into());
            }
            Ok(Cmd::Revoke {
                name: want_name(rest.first())?,
            })
        }
        "add" | "rotate" => {
            let (mut name, mut stdin, mut raw) = (None, false, Vec::new());
            let mut it = rest.iter();
            while let Some(a) = it.next() {
                if a == "--stdin" {
                    stdin = true;
                } else if a == "--bind" {
                    raw.push(it.next().ok_or("--bind needs a value")?.clone());
                } else if let Some(v) = a.strip_prefix("--bind=") {
                    raw.push(v.to_owned());
                } else if a.starts_with('-') {
                    return Err(format!("unknown option `{a}`"));
                } else if name.is_none() {
                    name = Some(a.clone());
                } else {
                    return Err("only one secret name is allowed".into());
                }
            }
            let name = want_name(name.as_ref())?;
            if sub == "rotate" {
                if !raw.is_empty() {
                    return Err("`rotate` keeps the existing bindings; --bind is for `add`".into());
                }
                return Ok(Cmd::Rotate { name, stdin });
            }
            if raw.is_empty() {
                return Err("`add` needs at least one --bind (a secret must say where it may go)".into());
            }
            let mut bindings = Vec::new();
            for b in &raw {
                bindings.push(Binding::parse(b).map_err(|_| {
                    format!(
                        "bad binding `{b}`: use host:<host>[:port], url:<scheme>://<host>[:port]/<path> or app:<id>"
                    )
                })?);
            }
            Ok(Cmd::Add {
                name,
                bindings,
                stdin,
            })
        }
        other => Err(format!("unknown subcommand `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Cmd, String> {
        parse(&s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
    }

    #[test]
    fn simple_subcommands() {
        assert_eq!(p("status"), Ok(Cmd::Status));
        assert_eq!(p("init"), Ok(Cmd::Init));
        assert_eq!(p("unlock"), Ok(Cmd::Unlock));
        assert_eq!(p("lock"), Ok(Cmd::Lock));
        assert_eq!(p("list"), Ok(Cmd::List));
        assert_eq!(p("--help"), Ok(Cmd::Help));
        assert_eq!(p("add --help"), Ok(Cmd::Help));
        assert!(p("status extra").is_err());
        assert!(p("").is_err());
        assert!(p("frobnicate").is_err());
    }

    #[test]
    fn add_parses_and_validates_bindings() {
        let Ok(Cmd::Add {
            name,
            bindings,
            stdin,
        }) = p("add anthropic-api-key --bind host:api.anthropic.com --bind=url:https://example.com/v1")
        else {
            panic!("did not parse");
        };
        assert_eq!(name, "anthropic-api-key");
        assert_eq!(bindings.len(), 2);
        assert!(!stdin);
        assert!(p("add k --bind host:a.com --stdin").is_ok_and(|c| matches!(c, Cmd::Add { stdin: true, .. })));
        assert!(p("add k").is_err(), "no binding");
        assert!(p("add k --bind").is_err());
        assert!(p("add k --bind nonsense").is_err());
        assert!(p("add k --bind host:Evil.com.").is_err());
        assert!(p("add bad/name --bind host:a.com").is_err());
        assert!(
            p("add k --bind host:a.com --value hunter2").is_err(),
            "no value flag"
        );
    }

    #[test]
    fn rotate_and_revoke() {
        assert_eq!(
            p("rotate k --stdin"),
            Ok(Cmd::Rotate {
                name: "k".into(),
                stdin: true
            })
        );
        assert!(p("rotate k --bind host:a.com").is_err());
        assert_eq!(p("revoke k"), Ok(Cmd::Revoke { name: "k".into() }));
        assert!(p("revoke").is_err());
        assert!(p("revoke a b").is_err());
    }

    #[test]
    fn help_shows_first_run_sequence() {
        assert!(HELP.contains(
            "ec-secret init\n  ec-secret unlock\n  ec-secret add anthropic-api-key --bind host:api.anthropic.com"
        ));
    }
}
