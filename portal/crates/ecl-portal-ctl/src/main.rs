// SPDX-License-Identifier: AGPL-3.0-only

//! `ecl-portal-ctl`: inspect and revoke portal grants.
//!
//! ```text
//! ecl-portal-ctl list
//! ecl-portal-ctl revoke <app> [table]
//! ```

use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use ecl_portal_store::{data_root, Grant, Standing, Store};

#[derive(Debug, PartialEq, Eq)]
enum Cmd {
    List,
    Revoke { app: String, table: Option<String> },
}

fn parse(args: &[String]) -> Result<Cmd> {
    match args {
        [c] if c == "list" => Ok(Cmd::List),
        [c, app] if c == "revoke" => Ok(Cmd::Revoke {
            app: app.clone(),
            table: None,
        }),
        [c, app, table] if c == "revoke" => Ok(Cmd::Revoke {
            app: app.clone(),
            table: Some(table.clone()),
        }),
        _ => bail!("usage: ecl-portal-ctl list | revoke <app> [table]"),
    }
}

fn standing(s: Standing) -> &'static str {
    match s {
        Standing::Allow => "allow",
        Standing::Deny => "deny",
        Standing::AskEveryTime => "ask-every-time",
    }
}

fn line(g: &Grant, now: u64) -> String {
    let expiry = match g.expires_at {
        None => "never".to_owned(),
        Some(e) if e <= now => "expired".to_owned(),
        Some(e) => format!("in {}s", e - now),
    };
    let res = if g.resource.is_empty() {
        "*"
    } else {
        &g.resource
    };
    format!(
        "{}\t{}\t{}\t{}\texpires {}",
        g.app,
        g.table,
        res,
        standing(g.standing),
        expiry
    )
}

fn run(args: &[String]) -> Result<String> {
    let cmd = parse(args)?;
    let root = data_root(
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
        std::env::var("HOME").ok().as_deref(),
    )
    .context("neither XDG_DATA_HOME nor HOME is an absolute path")?;
    let store = Store::new(root);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    execute(&store, cmd, now)
}

fn execute(store: &Store, cmd: Cmd, now: u64) -> Result<String> {
    match cmd {
        Cmd::List => {
            let mut out = String::new();
            for g in store.list_grants() {
                out.push_str(&line(&g, now));
                out.push('\n');
            }
            for t in store.tables() {
                for id in store.list(&t)? {
                    if let Some(e) = store.lookup(&t, &id)? {
                        for (app, perms) in e.permissions {
                            out.push_str(&format!("{app}\tstore:{t}\t{id}\t{}\n", perms.join(",")));
                        }
                    }
                }
            }
            Ok(out)
        }
        Cmd::Revoke { app, table } => {
            let n = store.revoke(&app, table.as_deref())?
                + store.revoke_permissions(&app, table.as_deref())?;
            Ok(format!("revoked {n} record(s) for {app}\n"))
        }
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(s) => {
            print!("{s}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("ecl-portal-ctl: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parsing() {
        assert_eq!(parse(&a(&["list"])).unwrap(), Cmd::List);
        assert_eq!(
            parse(&a(&["revoke", "app", "screencast"])).unwrap(),
            Cmd::Revoke {
                app: "app".into(),
                table: Some("screencast".into())
            }
        );
        assert!(parse(&a(&[])).is_err());
        assert!(parse(&a(&["revoke"])).is_err());
        assert!(parse(&a(&["list", "x"])).is_err());
    }

    #[test]
    fn list_and_revoke_round_trip() {
        let d = tempfile::tempdir().unwrap();
        let s = Store::new(d.path().join("ecl-portal"));
        s.grant(Grant {
            app: "org.x.App".into(),
            table: "screencast".into(),
            resource: String::new(),
            standing: Standing::Allow,
            granted_at: 1,
            expires_at: Some(100),
        })
        .unwrap();
        let out = execute(&s, Cmd::List, 40).unwrap();
        assert_eq!(out, "org.x.App\tscreencast\t*\tallow\texpires in 60s\n");
        let out = execute(
            &s,
            Cmd::Revoke {
                app: "org.x.App".into(),
                table: None,
            },
            40,
        )
        .unwrap();
        assert!(out.starts_with("revoked 1 "));
        assert_eq!(execute(&s, Cmd::List, 40).unwrap(), "");
    }
}
