pub mod search;

pub use rs_exec::background_tasks;
pub use rs_exec::daemon;
pub use rs_exec::rpc_client;
pub use rs_exec::runner;
pub use rs_exec::runtime;

pub use rs_codeinsight::{analyze, collect_files, matches_ignore_pattern, AnalyzeOptions, AnalysisOutput};
pub use search::{build_bm25_index, bm25_search, Bm25Index, SearchResult};
