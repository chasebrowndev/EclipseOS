// SPDX-License-Identifier: AGPL-3.0-only
//! passwd(5) lookup by parsing the file: no NSS, no environment. Used for the
//! seed's caller (the live `/etc/passwd`) and for the new user (the target's).

use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PwEntry {
    pub name: String,
    pub uid: u32,
    pub gid: u32,
    pub home: PathBuf,
}

fn parse(line: &str) -> Option<PwEntry> {
    let mut f = line.split(':');
    let name = f.next()?;
    let _pw = f.next()?;
    let uid = f.next()?.parse().ok()?;
    let gid = f.next()?.parse().ok()?;
    let _gecos = f.next()?;
    let home = f.next()?;
    let _shell = f.next()?;
    if name.is_empty() || !home.starts_with('/') || home.contains('\0') {
        return None;
    }
    Some(PwEntry {
        name: name.to_owned(),
        uid,
        gid,
        home: PathBuf::from(home),
    })
}

pub fn by_uid(text: &str, uid: u32) -> Option<PwEntry> {
    text.lines().filter_map(parse).find(|e| e.uid == uid)
}

pub fn by_name(text: &str, name: &str) -> Option<PwEntry> {
    text.lines().filter_map(parse).find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PW: &str = "root:x:0:0:root:/root:/bin/bash\n\
                      # junk\n\
                      liveuser:x:1000:1000::/home/liveuser:/bin/bash\n\
                      bad:x:notanumber:1::/home/bad:/bin/sh\n\
                      relhome:x:1001:1001::home/relhome:/bin/sh\n";

    #[test]
    fn lookups() {
        assert_eq!(by_uid(PW, 1000).unwrap().name, "liveuser");
        assert_eq!(
            by_name(PW, "liveuser").unwrap().home,
            PathBuf::from("/home/liveuser")
        );
        assert_eq!(by_uid(PW, 0).unwrap().name, "root");
        assert!(by_uid(PW, 4242).is_none());
        assert!(by_name(PW, "bad").is_none());
        assert!(by_uid(PW, 1001).is_none(), "relative home is refused");
    }
}
