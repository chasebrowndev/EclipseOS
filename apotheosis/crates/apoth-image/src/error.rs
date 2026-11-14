// SPDX-License-Identifier: AGPL-3.0-only
//! The closed error alphabet. Reply text is generated from these variants only; attacker
//! bytes (paths, keys, payload) are never echoed (T-18 §2).

/// Kitty graphics error codes with their fixed reply text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrCode {
    /// Malformed or unsupported request.
    Einval,
    /// Image or placement not found.
    Enoent,
    /// File/temp/shm transmission failed any check. One identical reply for every cause so
    /// a program cannot probe for paths (T-15 §1).
    EnoentFile,
    /// Quota exhausted and nothing evictable.
    Enospc,
    /// Image larger than the decoded or axis cap.
    Efbig,
    /// PNG did not decode.
    Ebadpng,
    /// Payload missing, truncated, or inflate failed.
    Enodata,
    /// Relative placement chain deeper than 8.
    Etoodeep,
    /// Relative placement cycle.
    Ecycle,
    /// Relative placement parent missing.
    Enoparent,
}

impl ErrCode {
    /// The complete reply body after the `;`, e.g. `ENOENT:file not readable`.
    pub fn reply_text(self) -> &'static str {
        match self {
            Self::Einval => "EINVAL:invalid request",
            Self::Enoent => "ENOENT:no such image",
            Self::EnoentFile => "ENOENT:file not readable",
            Self::Enospc => "ENOSPC:image quota exhausted",
            Self::Efbig => "EFBIG:image too large",
            Self::Ebadpng => "EBADPNG:png decode failed",
            Self::Enodata => "ENODATA:bad or missing data",
            Self::Etoodeep => "ETOODEEP:placement chain too deep",
            Self::Ecycle => "ECYCLE:placement cycle",
            Self::Enoparent => "ENOPARENT:parent placement missing",
        }
    }
}
