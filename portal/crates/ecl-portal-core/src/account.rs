// SPDX-License-Identifier: AGPL-3.0-only

//! `GetUserInformation` source data from `/etc/passwd` (no AccountsService).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PwEntry {
    pub name: String,
    pub uid: u32,
    /// First GECOS field.
    pub real_name: String,
    pub home: String,
}

/// The entry for `uid` in passwd text; malformed lines are skipped.
pub fn lookup_uid(passwd: &str, uid: u32) -> Option<PwEntry> {
    passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 6 || f[2].parse::<u32>().ok()? != uid {
            return None;
        }
        Some(PwEntry {
            name: f[0].to_owned(),
            uid,
            real_name: f[4].split(',').next().unwrap_or("").to_owned(),
            home: f[5].to_owned(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_uid() {
        let t = "root:x:0:0:root:/root:/bin/bash\n# junk\nbad\nrisk:x:1000:1000:Risk Q,,,:/home/risk:/bin/zsh\n";
        let e = lookup_uid(t, 1000).unwrap();
        assert_eq!(e.name, "risk");
        assert_eq!(e.real_name, "Risk Q");
        assert_eq!(e.home, "/home/risk");
        assert_eq!(lookup_uid(t, 5), None);
    }
}
