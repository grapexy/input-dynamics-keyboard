//! Durable state schemas and helpers for complete capture sessions.

// Checkpoint 2 intentionally lands these helpers before command wiring. Remove
// this allowance when umbrella session commands start using the module.
#![cfg_attr(not(test), allow(dead_code))]

pub(crate) mod io;
pub(crate) mod paths;
pub(crate) mod schema;

#[cfg(test)]
mod tests;
