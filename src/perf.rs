use std::process::Command;
use std::time::Duration;

use nix::sys::signal;
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use solana_accounts_db::accounts_db::AccountsDbConfig;
use solana_accounts_db::accounts_db::CreateAncientStorage;
use solana_accounts_db::accounts_file::StorageAccess;
use solana_accounts_db::accounts_index::AccountsIndexConfig;
use solana_accounts_db::accounts_index::BINS_FOR_TESTING;
use solana_accounts_db::partitioned_rewards::TestPartitionedEpochRewards;

use crate::Benchmark;
use crate::MemoryLocation;

use solana_accounts_db::accounts_db::AccountShrinkThreshold;
use solana_accounts_db::accounts_db::AccountsDb;
use solana_accounts_db::accounts_file::ALIGN_BOUNDARY_OFFSET;
use solana_accounts_db::ancestors::Ancestors;
use solana_accounts_db::u64_align;
use solana_sdk::account::AccountSharedData;
use solana_sdk::account::ReadableAccount;
use solana_sdk::genesis_config::ClusterType;
use solana_sdk::rent_collector::RentCollector;
use std::fmt::Result;
use std::sync::Arc;

use solana_accounts_db::accounts_db::StorageSizeAndCountMap;
use solana_accounts_db::accounts_index::IndexLimitMb;
use solana_accounts_db::accounts_index_storage::Startup;

pub enum PerfBenchType {
    Read,
    Write,
}

#[inline(always)]
fn time_process_with_perf<F>(mut f: F, bench_type: PerfBenchType) -> Duration
where
    F: FnMut() -> Duration,
{

    let pid = nix::unistd::getpid();

    let perf_file_name = match bench_type {
        PerfBenchType::Read => format!("read.data"),
        PerfBenchType::Write => format!("write.data"),
    };

    let perf_child = Command::new("perf")
        .arg("record")
        .arg("-g")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg(perf_file_name)
        // .arg("--freq=max")
        .spawn()
        .unwrap();

    let perf_pid = Pid::from_raw(perf_child.id() as i32);

    println!("Bench pid: {} perf pid: {}", pid, perf_pid);
    // Main program
    let duration = f();

    // Signal perf to finish
    match signal::kill(perf_pid, Signal::SIGINT) {
        Ok(_) => {
            println!("Signal sent successfully");
        }
        Err(err) => {
            eprintln!("Error sending signal: {}", err);
        }
    }

    // Wait for perf to finish
    nix::sys::wait::waitpid(perf_pid, None).unwrap();

    duration
}



pub fn run_disk_accounts_benchmark(benchmark: Benchmark) -> Result {
    println!("benchmark {:?}", benchmark);

    // let flush_threads = Some(std::cmp::max(2, num_cpus::get() / 4));
    let flush_threads = Some(1);

    let accounts_index_bench_config: AccountsIndexConfig = AccountsIndexConfig {
        bins: Some(BINS_FOR_TESTING),
        flush_threads,
        drives: None,
        index_limit_mb: IndexLimitMb::Unspecified,
        ages_to_stay_in_cache: None,
        scan_results_limit_bytes: None,
        started_from_validator: false,
    };

    let mut config =  AccountsDbConfig {
        index: Some(accounts_index_bench_config),
        base_working_path: None,
        accounts_hash_cache_path: None,
        shrink_paths: None,
        read_cache_limit_bytes: None,
        write_cache_limit_bytes: None,
        ancient_append_vec_offset: None,
        skip_initial_hash_calc: false,
        exhaustively_verify_refcounts: false,
        create_ancient_storage: CreateAncientStorage::Pack,
        test_partitioned_epoch_rewards: TestPartitionedEpochRewards::None,
        test_skip_rewrites_but_include_in_bank_hash: false,
        storage_access: StorageAccess::Mmap,
    };
    println!("flush_threads: {:?}", flush_threads);
    
    assert!(benchmark.accounts == MemoryLocation::Disk);
    if benchmark.index == MemoryLocation::Ram {
        // use RAM indexes
        config.index.as_mut().unwrap().index_limit_mb = IndexLimitMb::InMemOnly;
    } else {
        // use DISK indexes
        config.index.as_mut().unwrap().index_limit_mb = IndexLimitMb::Unspecified;
    }

    let n_accounts = benchmark.n_accounts;
    let slot_list_len = benchmark.slot_list_len;
    let accounts_db = AccountsDb::new_with_config(
        vec![],
        &ClusterType::Development,
        solana_accounts_db::accounts_index::AccountSecondaryIndexes::default(),
        AccountShrinkThreshold::default(),
        Some(config),
        None,
        Arc::default(),
    );
    let mut pubkeys = vec![];
    for _ in 0..n_accounts {
        pubkeys.push(solana_sdk::pubkey::new_rand());
    }

    let mut slots_used = 0;

    let static_account_size = 136;
    let mut total_size = 0;
    for i in 0..n_accounts {
        total_size += static_account_size + (i % 1_000);
        total_size = u64_align!(total_size);
    }
    let total_size_u64 = total_size as u64;

    for s in 0..(slot_list_len + benchmark.n_accounts_multiple) {
        let storage = accounts_db.create_and_insert_store(s as u64, total_size_u64, "blah");
        for i in 0..n_accounts {
            let account =
                AccountSharedData::new(10, i % 1_000, AccountSharedData::default().owner());

            let account_inner = &[(&pubkeys[i % n_accounts], &account)][..];
            let accounts = (s as u64, account_inner);
            storage.accounts.append_accounts(&accounts, 0);
        }
    }

    let bins = accounts_db.accounts_index.bins();
    accounts_db.accounts_index.set_startup(Startup::Startup);

    let mut store_id = 0;
    accounts_db.add_root(0);

    for s in 0..benchmark.n_accounts_multiple {
        let storage = accounts_db
            .storage
            .get_slot_storage_entry(s as u64)
            .unwrap();
        let storage_info = StorageSizeAndCountMap::default();
        accounts_db.generate_index_for_slot(
            &storage,
            s as u64,
            store_id,
            &RentCollector::default(),
            &storage_info,
        );
        store_id += 1;
    }
    (0..bins).for_each(|pubkey_bin| {
        let r_account_maps = &accounts_db.accounts_index.account_maps[pubkey_bin];
        r_account_maps.write_startup_info_to_disk();
    });

    let writes = || {
        let timer = std::time::Instant::now();

        for s in benchmark.n_accounts_multiple..(benchmark.n_accounts_multiple + slot_list_len) {
            let storage = accounts_db
                .storage
                .get_slot_storage_entry(s as u64)
                .unwrap();
            let storage_info = StorageSizeAndCountMap::default();
            accounts_db.generate_index_for_slot(
                &storage,
                s as u64,
                store_id,
                &RentCollector::default(),
                &storage_info,
            );
            store_id += 1;
        }
        (0..bins).for_each(|pubkey_bin| {
            let r_account_maps = &accounts_db.accounts_index.account_maps[pubkey_bin];
            r_account_maps.write_startup_info_to_disk();
        });

        slots_used = slot_list_len + benchmark.n_accounts_multiple;
        timer.elapsed()
    };

    let write_elapsed = time_process_with_perf(writes, PerfBenchType::Write);
    println!("WRITE: {}", write_elapsed.as_nanos());

    let mut slots = vec![];
    for slot in 0..slots_used {
        slots.push(slot as u64);
    }
    let ancestors = Ancestors::from(slots);

    let reads = || {
        let timer = std::time::Instant::now();
        for i in 0..n_accounts {
            let (account, _) = accounts_db
                .load_without_fixed_root(&ancestors, &pubkeys[i])
                .unwrap();
            assert_eq!(account.data().len(), i % 1_000);
        }

        timer.elapsed()
    };

    let read_elapsed = time_process_with_perf(reads, PerfBenchType::Read);

    println!("READ: {}", read_elapsed.as_nanos());

    return Ok(());
}
