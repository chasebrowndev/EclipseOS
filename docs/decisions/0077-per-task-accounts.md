# 0077 — Per-task accounts, a captured Claude Code login, and the store prompt
Status: accepted
Date: 2026-10-08
Deciders: chase (owner), Claude (advisory)

## Context
ADR 0076 gives the router two credentials: one `anthropic-api-key` and one
`claude-code-token`. In practice the owner hit three problems:

- **Copying a token by hand.** `claude setup-token` prints the token for the
  human to copy into `ec-secret add`. That is a lost token waiting to happen,
  and it puts the token on screen.
- **Several accounts.** The owner wants more than one account (work and
  personal, say) and wants to switch between them easily. Each task should
  run on the account the human picks, not on one global setting.
- **No GUI.** brokerd could only be set up and unlocked from a TTY. S-08 §2
  wants it unlocked "at first human login of the session, via trusted UI",
  and ADR 0076 noted there was no unlock dialog.

## Options
1. **A global "current account" setting** that the router reads. It is
   simple, but switching accounts changes running tasks behind the human's
   back, and nothing on the card says which account a task bills.
2. **The agent names an account.** This breaks ADR 0076: inference
   parameters never come from the agent.
3. **The account is part of the task draft** the human commits. It is shown
   on the commit card, carried by policyd to agentd in the provision, and
   handed to the router in each request.

## Decision
Option 3.

- **Names.** An account is empty (the default) or 1 to 32 characters from
  `[A-Za-z0-9_-]` (`ec_policy_eval::link::account_ok`,
  `ec_inference_wire::account_ok`). Its brokerd secret is the base name for
  the default account, and `<base>.<account>` otherwise. The bases are
  `claude-code-token` and `anthropic-api-key`, so a token stored before this
  ADR is the default account. brokerd's name rule already allows the `.`.
- **The path.**
  1. The console's composer.
  2. `eclipse_commit_slot_v1.set_account`, a request appended in version 2
     (COMP-19 §2). It is part of the draft, survives `set_draft`, re-previews,
     and has a `bad_account` error.
  3. `PreviewTask.account`. policyd checks the name again, puts it in the
     card display, and keeps it in the preview.
  4. The card draws `Account: <name>` beside the deadline, and the modal card
     lists it, for any account other than the default.
  5. `Provision.account`.
  6. agentd's task.
  7. `Request.account`.
  8. The router picks the secret by name.

  The commit slot's draft hash covers the account, so the slot audit records
  which account was committed.
- **The account is the human's choice, never the agent's.** agentd still
  takes the backend and model from the manifest and the account from
  policyd's provision. Nothing in an MCP call can name one.
- **Login capture.**
  1. `ec-secret account login <account>` runs `claude setup-token` under a
     pty, with a private, temporary `CLAUDE_CONFIG_DIR`.
  2. It relays the sign-in URL and the code the human pastes back.
  3. It scrapes the token from the pty output and stores it with `Add` (or
     `Rotate`).

  The token is never printed. `--json` turns the flow into line events for
  Settings → Accounts.
- **API keys from the GUI** go through `ec-secret-prompt api-key <account>`,
  the ADR 0053 secret-class window, which accepts a paste. It pipes the key to
  `ec-secret account add-key --stdin`. A compositor-drawn entry cannot take a
  paste, and an API key is not typed by hand.
- **The store prompt is compositor-drawn** (`trusted_ui/broker.rs`). abyss
  connects to brokerd as its Compositor peer once the agents add-on is on, and
  keeps that one connection for the session; brokerd locks when it closes.
  - At login, a store that exists and is locked prompts for its passphrase.
  - `secrets_unlock_prompt` (control socket, `Kind::Command`) prompts on
    demand: setup, entering the passphrase twice, when there is no store, and
    unlock when the store is locked. The console reaches it through agentd's
    `unlock_secrets`.
  - Screen lock locks the store (S-08 §2), and the next session unlock asks
    again.
  - The entry is masked. The typed text zeroizes on drop and is never logged.

## Consequences
- **No rename.** brokerd has no rename op and does not hand an Owner a value
  back, so renaming an account means removing it and logging in again.
- **The protocol's interfaces are at version 2.** A v1 client still works and
  only ever commits on the default account.
- **The policyd link message shape changed** (`account` on `preview_task` and
  `provision`). abyss, policyd and agentd update together, as they already
  must.
- **Login capture depends on the shape of `claude setup-token`'s output**: the
  URL as an OSC 8 link, a "Paste code here" prompt, and an `sk-ant-oat01-`
  token. A change there fails closed with `no_token`, and the parser's tests
  pin today's bytes.
- **The passphrase prompt draws printable ASCII only.** A store sealed from a
  TTY with other characters is still unlocked there.
- **policyd's task record does not carry the account.** The slot audit's
  draft hash does. A query for "which tasks billed account X" is future work.

## Revisit when
- The egress proxy (S-09) lands and takes over credential release.
- A third backend or a non-Anthropic account kind appears.
- brokerd grows a rename op.
