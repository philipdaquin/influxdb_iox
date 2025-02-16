//! IOx Ingester V2 implementation.
//!
//! ## Write Reordering
//!
//! A write that enters an `ingester2` instance can be reordered arbitrarily
//! with concurrent write requests.
//!
//! For example, two gRPC writes can race to be committed to the WAL, and then
//! race again to be buffered into the [`BufferTree`]. Writes to a
//! [`BufferTree`] may arrive out-of-order w.r.t their assigned sequence
//! numbers.
//!
//! This can also lead to the ordering of entries in the [`wal`] diverging from
//! the order of ops applied to the [`BufferTree`] (see
//! <https://github.com/influxdata/influxdb_iox/issues/6276>).
//!
//! This requires careful management, but ultimately allows for high levels of
//! parallelism when handling both writes and queries, increasing the
//! performance of both.
//!
//! Because of re-ordering, ranges of [`SequenceNumber`] cannot be used to
//! indirectly equality match (nor prefix match) the underlying data;
//! non-monotonic writes means overlapping ranges do not guarantee equality of
//! the set of operations they represent (gaps may be present). For example, a
//! range of sequence numbers bounded by `[0, 2)` for thread 1 may not contain
//! the same op data as another thread T2 with range with the same bounds due to
//! reordering causing T1 to observe `{0, 1, 2}` and T2 observing `{0, 2}` and
//! after a faulty range comparison, `{1}` to converge.
//!
//! [`BufferTree`]: crate::buffer_tree::BufferTree
//! [`SequenceNumber`]: data_types::SequenceNumber

#![allow(dead_code)] // Until ingester2 is used.
#![deny(rustdoc::broken_intra_doc_links, rust_2018_idioms)]
#![warn(
    clippy::clone_on_ref_ptr,
    clippy::dbg_macro,
    clippy::explicit_iter_loop,
    clippy::future_not_send,
    clippy::todo,
    clippy::use_self,
    missing_copy_implementations,
    missing_debug_implementations,
    missing_docs,
    unreachable_pub
)]

use data_types::{ShardId, ShardIndex};

/// During the testing of ingester2, the catalog will require a ShardId for
/// various operations. This is a const value for these occasions.
const TRANSITION_SHARD_ID: ShardId = ShardId::new(1);
const TRANSITION_SHARD_INDEX: ShardIndex = ShardIndex::new(1);

/// Ingester initialisation methods & types.
///
/// This module defines the public API boundary of the Ingester crate.
mod init;
pub use init::*;

//
// !!! PLEASE DO NOT EXPORT !!!
//
// Please be judicious when deciding if something should be visible outside this
// crate. Overzealous use of `pub` exposes the internals of an Ingester and
// causes tight coupling with other components. I know it's tempting, but it
// causes problems in the long run.
//
// Ideally anything that NEEDS to be shared with other components happens via a
// shared common crate. Any external components should interact with an Ingester
// through its public API only, and not by poking around at the internals.
//

mod arcmap;
mod buffer_tree;
mod deferred_load;
mod dml_sink;
mod persist;
mod query;
mod query_adaptor;
pub(crate) mod server;
mod timestamp_oracle;
mod wal;

#[cfg(test)]
mod test_util;
