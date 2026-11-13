//! Ita Link core: the link model, the owner's rules and the relay engine.
//! No networking happens here - events go in, actions come out.

pub mod command;
pub mod engine;
pub mod face;
pub mod model;
pub mod msg;
pub mod policy;
