// SPDX-License-Identifier: Apache-2.0
//! Generated bindings for EclipseOS's own Wayland protocols (F-05 §3).
//!
//! `eclipse_agent_v1` (COMP-08) is the privileged agent protocol. abyss
//! links the `server` side; agentd and agent SDKs link `client`.

/// `eclipse_agent_manager_v1`, `eclipse_agent_v1`, `eclipse_scene_v1`.
pub mod agent {
    #[cfg(feature = "client")]
    pub mod client {
        #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
        #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
        #![allow(missing_docs, clippy::all)]
        use wayland_client;
        use wayland_client::protocol::*;

        pub mod __interfaces {
            use wayland_client::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/eclipse-agent-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_client_code!("protocols/eclipse-agent-v1.xml");
    }

    #[cfg(feature = "server")]
    pub mod server {
        #![allow(dead_code, non_camel_case_types, unused_unsafe, unused_variables)]
        #![allow(non_upper_case_globals, non_snake_case, unused_imports)]
        #![allow(missing_docs, clippy::all)]
        use wayland_server;
        use wayland_server::protocol::*;

        pub mod __interfaces {
            use wayland_server::protocol::__interfaces::*;
            wayland_scanner::generate_interfaces!("protocols/eclipse-agent-v1.xml");
        }
        use self::__interfaces::*;

        wayland_scanner::generate_server_code!("protocols/eclipse-agent-v1.xml");
    }
}
