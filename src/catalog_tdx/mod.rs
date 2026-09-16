//! Bounded parsers for the #168-anchored official-file catalog members
//! (design: `docs/tasks/issue-6-catalog-impl.md` §2).
//!
//! This module only parses member bytes and applies the frozen scope rule.
//! It performs no archive I/O, no snapshot publication, no name/industry
//! join assembly, and no storage writes — those belong to later slices gated
//! by the two pending #6 product decisions. Anchored semantics are exactly
//! those of `evidence/issue-46-ashare-contract/ashare-directory-contract-v1.md`;
//! nothing here assigns meaning to UNANCHORED members (base.dbf, tdxhy2.cfg,
//! infoharbor_ex.name) or to `tdxhy.cfg` fields f3/f4.

pub mod compose;
pub mod ex_code;
pub mod incon;
pub mod scope;
pub mod tdxhy;
