<!-- SPDX-License-Identifier: AGPL-3.0-only -->
# D-02 — Package repository and signing (reduced)

Status: **written 2026-09-18**, reduced scope. Depends on D-01 (written).

Governing specs: Tier 6 D-02. Constrained by F-05 (licensing), F-07 (process).

**This is the reduced D-02.** The full document is blocked on S-12 (supply
chain), which is unwritten, and S-12's requirements — reproducible builds, a
transparency log, multi-party signing, mirror policy — are all statements about
a *public* distribution. The first EclipseOS repository serves an audience of
one over a private tailnet (F-01 charter decision, 2026-09-18). This document
writes down what that audience actually needs and names what it drops, so that
the gap is a recorded decision rather than an oversight.

**Dropped, and to be restored by the full D-02 before anyone else installs
EclipseOS:** reproducible builds, build attestation, a second signer, key
rotation and revocation procedure, public mirrors, mirror-status monitoring,
and any claim that a package's provenance can be verified by someone who does
not own the build machine.

## 1. Shape

A plain `repo-add` pacman repository. No `dbscripts`, no staging/testing split,
no build server: one architecture (`x86_64`), one repository (`eclipseos`), one
machine (`chase-pc`) that builds it.

```
~/.local/share/eclipseos/repo/
  eclipseos-packaging.asc        the public key clients import
  x86_64/
    eclipseos.db.tar.zst{,.sig}  signed database
    eclipseos.files.tar.zst{,.sig}
    *.pkg.tar.zst{,.sig}
```

The path is under `~/.local/share` rather than `/srv` deliberately: nothing in
the publish path needs root, so nothing in it runs as root.

## 2. Signing key

A dedicated packaging key, ed25519, five-year expiry, UID
`EclipseOS Packaging <packaging@eclipseos.invalid>`.

It is **not** the owner's personal GPG identity, and it lives in its own keyring
at `~/.local/share/eclipseos/gnupg` (`GNUPGHOME` is exported by the build
script). `~/.gnupg` is never read or written by any part of this system. The
separation is what makes it possible to revoke or rotate the packaging key later
without touching a personal web of trust.

The key is generated on first run of `dist/repo/build-repo.sh` if absent. It has
no passphrase, because an unattended build on a machine the owner already
controls gains nothing from one: an attacker with the private key file has the
build machine, and a passphrase in that position protects nothing.

**This is the single point of failure of the scheme, and it is accepted for an
audience of one.** The full D-02 replaces it.

## 3. Trust, in two places

The two `pacman.conf` fragments deliberately disagree, and the disagreement is
the design:

| Context | Source | `SigLevel` | Why |
|---|---|---|---|
| ISO build (`dist/iso/pacman.conf`) | `file:///…/repo/x86_64` | `Optional TrustAll` | At build time the packages are local files this machine produced seconds earlier. The filesystem *is* the trust boundary. Requiring signatures here would mean seeding the packaging key into `mkarchiso`'s keyring, which is the usual fragile step, to verify a claim already established. |
| Installed system (`/etc/pacman.d/eclipseos.conf`, shipped by `eclipseos-meta`) | `http://chase-pc:8088/x86_64` | `Required DatabaseRequired` | Here the packages cross a network. Both the database and every package must be signed by the packaging key. |

Plain HTTP is sufficient for the served repo because the transport is WireGuard:
tailscale authenticates and encrypts the link, and signatures cover integrity on
top. A machine that is not on the tailnet fails to sync this one repository and
still updates normally from Arch.

`serve.sh` binds to the tailnet address from `tailscale ip -4`, not `0.0.0.0`.
That binding is the whole privacy story; do not "fix" it to listen everywhere.

## 4. Publishing

```
dist/repo/build-repo.sh v0.1.0    # tag -> build -> sign -> repo-add
dist/repo/serve.sh                # serve on the tailnet
```

`build-repo.sh` takes a git tag, not a working tree: the PKGBUILD's `source=` is
`git+…#tag=v$pkgver`, so what gets published is what is tagged on `main` and
nothing else. An uncommitted local change cannot reach the repository, which is
the property worth having from a build script this small.

## 5. Client enrolment

Handled by the installer for a fresh install (`install-eclipseos.sh` adds the
`Include` line; `eclipseos-meta` ships the drop-in). For an existing machine:

```
sudo pacman-key --add eclipseos-packaging.asc
sudo pacman-key --lsign-key <fingerprint>
# then add:  Include = /etc/pacman.d/eclipseos.conf   to /etc/pacman.conf
```

`--lsign-key` is what makes `SigLevel = Required` pass; importing without
locally signing leaves the key untrusted and every install fails with an
unhelpful error. This is the step people forget.

## 6. What this does not do

No downgrades, no rollback, no pinning, no staged rollout. Those belong to D-04,
which is where the question "what happens when an upgrade breaks the session"
is answered. D-02's job ends when a signed package is reachable.
