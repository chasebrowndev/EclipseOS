// SPDX-License-Identifier: AGPL-3.0-only
//! `eclipse-setup-helper`: the root half of the graphical installer (D-07 §6).
//!
//! Reached through polkit (`org.eclipse.install.apply`), it takes one closed
//! `eclipse_setup_plan::Request` and does exactly one thing with it. It is small
//! on purpose: TCB-adjacent, reviewed line by line, no shell, no free-form input.
//!
//! Layout, in the order a request meets it:
//!
//! * `cli`      argv and stdin/stdout framing; two subcommands
//! * `apply`    stage order, `Confirm` gate, progress lines
//! * `validate` every refusal that needs no disk and no network
//! * `disks`    the helper's own disk listing; boot-medium exclusion
//! * `catalog`  candidate ids -> packages and units (built in for M1)
//! * `stages`   partition, pacstrap, configure, bootloader, user, units
//! * `target`   the closed list of files written on the target
//! * `hw`       CPU, GPU, RAM, battery from /proc and /sys
//! * `network`  the live Wi-Fi connections carried to the target
//! * `seed`     the carried-over `abyss.kdl`, filtered by allowlist
//! * `runner`   the closed set of external programs, fixed argv, no shell
//! * `confirm`  the wipe confirmation trait; `DenyConfirm` is the default

#![forbid(unsafe_code)]

pub mod apply;
pub mod catalog;
pub mod cli;
pub mod confirm;
pub mod disks;
pub mod env;
pub mod error;
pub mod hw;
pub mod network;
pub mod passwd;
pub mod runner;
pub mod seed;
pub mod stages;
pub mod target;
pub mod validate;

#[cfg(test)]
mod testutil;
