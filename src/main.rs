use perf::run_disk_accounts_benchmark;
use std::fmt::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryLocation {
    Ram,
    Disk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Benchmark {
    n_accounts: usize,
    slot_list_len: usize,
    accounts: MemoryLocation,
    index: MemoryLocation,
    n_accounts_multiple: usize,
}

pub mod perf;

pub fn main() -> Result {
    let benches = vec![
        // Benchmark {
        //     n_accounts: 1_000_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Disk,
        //     index: MemoryLocation::Ram,
        //     n_accounts_multiple: 0,
        // },
        // Benchmark {
        //     n_accounts: 1_000_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Disk,
        //     index: MemoryLocation::Disk,
        //     n_accounts_multiple: 0,
        // },
        // Benchmark {
        //     n_accounts: 5_000_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Disk,
        //     index: MemoryLocation::Ram,
        //     n_accounts_multiple: 0,
        // },
        Benchmark {
            n_accounts: 100_000,
            slot_list_len: 10,
            accounts: MemoryLocation::Disk,
            index: MemoryLocation::Disk,
            n_accounts_multiple: 0,
        },
    ];

    for benchmark in benches {
        // run_benchmark(benchmark)?;
        run_disk_accounts_benchmark(benchmark)?;
        println!("---");
    }

    return Ok(());
}
