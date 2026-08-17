//! Stable strategy-instance identity.
//!
//! Strategy `name`s are NOT unique — running several instances of one strategy
//! under the same name is a supported config pattern — so a name is never a
//! safe key for per-instance state. Keying by one lets an instance read, write
//! or delete a sibling's:
//!
//! * 2026-07-18, prod: name-keyed params made instances trade with a sibling
//!   entry's `orderSize`/`direction` (a margin storm);
//! * 2026-08-17, prod: name-keyed position snapshots meant one instance going
//!   flat **deleted its sibling's**, so a bot with `virtualSl` stopped
//!   declaring a virtual stop it was still enforcing — and the watchdog, which
//!   has only that declaration to go on, closed the position as unprotected.
//!
//! [`StratId`] makes both a type error. It lives here rather than in the
//! runner because the second failure was in [`crate::bot_state`], on the far
//! side of a crate boundary the runner's own newtype could not reach.

/// Stable strategy-instance identity — the ONLY legal key for per-instance
/// state (params cell, run status, active symbols, notify cell, pending
/// queue, lifecycle channels, trigger listeners, session labels, and every
/// per-instance map in [`crate::bot_state`]).
///
/// There is deliberately no `From<String>` / `From<&str>`: a name cannot be
/// turned into a key by accident. The only production constructors are
/// [`StratIdent::sid`] (a config entry's ensured id) and the runner's
/// `ConfigAdmin::resolve_strat_id` (a validated wire request).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StratId(String);

impl StratId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Construct from an id already known to be one — for the runner's
    /// wire-request resolver, which validates against the live config before
    /// calling, and for tests.
    ///
    /// Not a general escape hatch: passing a `name` here reintroduces exactly
    /// the class of bug this type exists to prevent. Reach for
    /// [`StratIdent::sid`] unless you are resolving an id off the wire.
    #[doc(hidden)]
    pub fn from_validated(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

impl std::fmt::Display for StratId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The entry-side [`StratId`] constructor. `StratEntry::id` stays
/// `Option<String>` for wire/file compatibility; the runner's
/// `ensure_strat_ids` guarantees `Some` on every loaded document before any
/// per-instance state exists, so `sid()` treats a missing id as a programming
/// error rather than a runtime condition.
pub trait StratIdent {
    fn sid(&self) -> StratId;
}

impl StratIdent for crate::types::config::StratEntry {
    fn sid(&self) -> StratId {
        StratId(
            self.id
                .clone()
                .expect("strategy ids are ensured at config load (ensure_strat_ids)"),
        )
    }
}
