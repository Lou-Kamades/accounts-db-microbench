/// cargo run --release --bin tmp --features="dev-context-only-utils"

use solana_accounts_db::accounts_db::AccountsDb;
use solana_accounts_db::accounts_db::ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
use solana_accounts_db::accounts_db::StorageSizeAndCount;
use solana_accounts_db::accounts_file::AccountsFile;
use solana_accounts_db::ancestors::Ancestors;
use solana_accounts_db::ancient_append_vecs::AccountsToStore;
use solana_sdk::rent_collector::RentCollector;
use solana_sdk::account;
use solana_sdk::account::Account;
use solana_sdk::account::AccountSharedData;
use solana_sdk::account::WritableAccount;
use solana_sdk::genesis_config::ClusterType;
use solana_accounts_db::accounts_db::AccountShrinkThreshold;
use std::borrow::Borrow;
use std::borrow::BorrowMut;
use std::fmt::Result;
use std::sync::Arc;
use solana_sdk::account::ReadableAccount;
use solana_sdk::pubkey::Pubkey;
use solana_accounts_db::u64_align;
use std::fs::OpenOptions;
use std::io::SeekFrom;
use solana_accounts_db::accounts_file::ALIGN_BOUNDARY_OFFSET;

use std::io::Seek;
use std::io::Write;
use memmap2::MmapMut;

use solana_accounts_db::append_vec::AppendVec;
use solana_accounts_db::accounts_db::AccountStorageEntry;

use solana_accounts_db::accounts_hash::AccountHash;
use solana_accounts_db::account_storage::meta::StoredMeta;
use solana_accounts_db::account_storage::meta::StorableAccountsWithHashesAndWriteVersions;
use solana_accounts_db::account_storage::meta::StoredAccountInfo;
use solana_sdk::slot_history::Slot;
use solana_sdk::hash::Hash;

use solana_accounts_db::accounts_db::StorageSizeAndCountMap;
use solana_accounts_db::accounts_index_storage::Startup;
use solana_accounts_db::accounts_index::IndexLimitMb;


use dashmap::mapref::entry::Entry;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryLocation {
    Ram,
    Disk
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Benchmark { 
    n_accounts: usize,
    slot_list_len: usize,
    accounts: MemoryLocation,
    index: MemoryLocation,
    n_accounts_multiple: usize,
}

pub fn run_benchmark(benchmark: Benchmark) -> Result {
    println!("benchmark {:?}", benchmark);
    let mut config = ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
    if (benchmark.index == MemoryLocation::Ram) {
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
    for i in 0..n_accounts { 
        pubkeys.push(solana_sdk::pubkey::new_rand());
    }

    let mut slots_used = 0;
    let total_n_accounts = slot_list_len * n_accounts;
    if (benchmark.accounts == MemoryLocation::Disk) {
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
                let mut account = AccountSharedData::new(
                    10, 
                    i % 1_000, 
                    AccountSharedData::default().owner()
                );

                let hashes = vec![AccountHash(Hash::default()); 1];
                let write_version = vec![0; 1];
                storage.accounts.append_accounts(
                    &StorableAccountsWithHashesAndWriteVersions::new_with_hashes_and_write_versions(
                        &(s as u64, &[(&pubkeys[i % n_accounts], &account)][..]),
                        hashes,
                        write_version,
                    ),
                    0,
                );
            }
        }

        let bins = accounts_db.accounts_index.bins();
        accounts_db.accounts_index.set_startup(Startup::Startup);

        let mut store_id = 0;
        accounts_db.add_root(0);

        for s in 0..benchmark.n_accounts_multiple {
            let storage = accounts_db.storage.get_slot_storage_entry(s as u64).unwrap();
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

        let timer = std::time::Instant::now();

        for s in benchmark.n_accounts_multiple..(benchmark.n_accounts_multiple + slot_list_len) {
            let storage = accounts_db.storage.get_slot_storage_entry(s as u64).unwrap();
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
        let elapsed = timer.elapsed();
        println!("WRITE elapsed: {}", elapsed.as_nanos());
    } else { 
        let mut accounts = vec![];
        let mut refs = vec![];

        for s in 0..(slot_list_len + benchmark.n_accounts_multiple) {
            for i in 0..n_accounts { 
                let mut account = AccountSharedData::new(
                    10, 
                    i % 1000, 
                    AccountSharedData::default().owner()
                );
                accounts.push(account);
            }
        }

        // initial account amounts 
        accounts_db.add_root(0);

        let mut slot = 0;
        let mut no_bench_refs = vec![];
        for i in 0..(n_accounts * benchmark.n_accounts_multiple) {
            no_bench_refs.push((&pubkeys[i % n_accounts], &accounts[i]));
        }
        if (no_bench_refs.len() > 0) { 
            accounts_db.store_for_tests(0 as u64, &no_bench_refs);
            slot += 1;
        } 

        // the rest
        for i in (n_accounts * benchmark.n_accounts_multiple)..(n_accounts * (slot_list_len + benchmark.n_accounts_multiple)) {
            refs.push((&pubkeys[i % n_accounts], &accounts[i]));
        }
        let timer = std::time::Instant::now();
        let mut index = 0;
        for s in slot..(slot_list_len + slot) {
            let start_index = index;
            let end_index = index + n_accounts;

            accounts_db.store_for_tests(s as u64, &refs[start_index..end_index]);
            index = end_index;
        }
        slot += slot_list_len;
        slots_used = slot;
        let elapsed = timer.elapsed();
        println!("WRITE elapsed: {}", elapsed.as_nanos());
    }

    let mut slots = vec![];
    for (slot) in 0..slots_used {
        slots.push(slot as u64); 
    }
    let ancestors = Ancestors::from(slots);
    let timer = std::time::Instant::now();
    for i in 0..n_accounts {
        let (account, _) = accounts_db.load_without_fixed_root(&ancestors, &pubkeys[i]).unwrap();
        assert_eq!(account.data().len(), i % 1_000);
    }

    let elapsed = timer.elapsed();
    println!("READ elapsed: {}", elapsed.as_nanos());

    return Ok(());
}

pub fn main() -> Result { 
    let benches = vec![
        Benchmark { 
            n_accounts: 100_000,
            slot_list_len: 1,
            accounts: MemoryLocation::Ram,
            index: MemoryLocation::Ram,
            n_accounts_multiple: 0,
        },
    ];

    for (benchmark) in benches {
        run_benchmark(benchmark)?;
        println!("---");
    }

    return Ok(());
}
