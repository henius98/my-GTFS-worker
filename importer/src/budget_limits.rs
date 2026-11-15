pub const DAILY_WRITE_LIMIT: u64 = 100_000;
pub const METADATA_WRITE_RESERVE: u64 = 1_000;
// One ledger update to acquire a lease and one to return unused capacity.
pub const LEDGER_WRITE_RESERVE: u64 = 2;
