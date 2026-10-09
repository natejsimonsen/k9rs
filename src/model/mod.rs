//! Table data model: mirrors k9s `internal/model1` (header, row, row events,
//! table data, deltas) plus filtering and sorting helpers used by the table view.

pub mod filter;
pub mod sort;
pub mod table;

pub use filter::*;
pub use sort::*;
pub use table::*;
