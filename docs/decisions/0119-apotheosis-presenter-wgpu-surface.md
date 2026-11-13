# 0119 — Presenter: the wgpu surface only
Status: proposed
Date: 2026-11-12
Deciders: chase (owner), Claude (advisory)

## Context
The plan sketched a second presenter that allocates dmabufs and attaches them itself, to get real damage rectangles and drop libwayland. Research (matrix GRAPHICS-37/38, Appendix A) found wgpu's external-memory export path unstable across releases, multi-plane import gaps, and that Vulkan WSI on Wayland requires libwayland regardless.

## Options
1. Own dmabuf presenter. Exact damage; a private swapchain, explicit sync and modifier negotiation to own forever.
2. wgpu surface only; minimise cost by presenting nothing when nothing changed and rendering only dirty rows into a persistent target.

## Decision
Option 2 (T-07). Frame pacing comes from our scheduler (frame callbacks, presentation-time, Mailbox, then Fifo paced by the WSI; Apotheosis never creates wp_fifo_v1/wp_commit_timer_v1 on a WSI-owned surface, T-07 §2), never from blocking in `present`. wgpu is pinned at `=30.0.2` with a Vulkan-only feature set. Milestone M38 becomes presentation refinements, not a second presenter.

## Consequences
Full-surface damage is reported for frames that are presented; the `IDLE`, `IDLE-BLINK` and `PWR-SCROLL` budgets must be met by not presenting. A request for incremental present is filed upstream.

## Revisit when
The power budgets cannot be met, wgpu stabilises a damage-aware present, or wgpu exposes explicit present timing or damage/partial present (M38).
