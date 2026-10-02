<!-- SPDX-License-Identifier: Apache-2.0 -->
# ec-protocols — EclipseOS Wayland protocol bindings

Governing spec: COMP-08 (`eclipse_agent_v1`). **Not TCB**: generated
marshalling only; every check lives in abyss (`protocols/agent/`, `policy/`).

- **Apache-2.0**, not AGPL (F-05 §3, §4): agents link the client side under
  any licence. SPDX `Apache-2.0` on every file, the XML included.
- `protocols/*.xml` is the contract. Changing a released interface needs a
  version bump (COMP-08 §11).
- `server` feature for abyss, `client` feature for agentd and SDKs.
