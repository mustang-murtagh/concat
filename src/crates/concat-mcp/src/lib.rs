// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer Abdullah

//! Concat for AI agents: a Model Context Protocol server over standard
//! input and output, shipped inside the app as `concat --mcp`.
//!
//! It is a thin adapter, and deliberately so. Every tool is a
//! [`concat_api::Request`] - a method of the API, or one of its edit
//! commands - and its argument schema is cut out of the schema `schemars`
//! generates from those types ([`schema`]), so an edit means here exactly
//! what it means in the window, over JSON-RPC and over gRPC, and a field
//! added to a command reaches agents on the next build with nothing
//! written for it here. What this crate owns is the protocol
//! ([`protocol`]), the choice of tools and what an agent is told about
//! them ([`tools`]), a compact reading of a project for an agent's context
//! ([`summary`]), and where requests go ([`backend`]): the window's own
//! server when the Automation page has it on, or an API in this process
//! when it does not.
//!
//! One family of tools is more than a request: the shorts tools
//! ([`shorts`]), which turn a long recording into short clips. They are a
//! job with a plan, kept by `concat-shorts`; what they change in a
//! project they change through the same backend, as requests.

pub mod backend;
pub mod protocol;
pub mod schema;
pub mod shorts;
pub mod summary;
pub mod tools;

pub use backend::{Backend, Headless, Mode, Window};

/// Serves MCP on standard input and output until the client closes its
/// end, through the backend `mode` picks. Logs to standard error, which
/// MCP clients show as the server's log; standard output carries nothing
/// but protocol.
pub fn run(mode: Mode) -> Result<(), String> {
    let backend = backend::open(mode)?;
    eprintln!("concat mcp: serving through {}", backend.describe());
    protocol::serve(
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
        backend,
    );
    Ok(())
}
