use std::sync::Arc;

use solana_accounts_db::accounts_db::StorageSizeAndCountMap;
use solana_accounts_db::accounts_db::ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
use solana_sdk::account::ReadableAccount; 
use solana_sdk::account::AccountSharedData;
use solana_sdk::genesis_config::ClusterType;
use solana_sdk::pubkey::Pubkey;
use solana_accounts_db::accounts_db::AccountsDb;
use solana_accounts_db::accounts_file::ALIGN_BOUNDARY_OFFSET;
use solana_accounts_db::accounts_index::IndexLimitMb;
use solana_accounts_db::accounts_index_storage::Startup;
use solana_accounts_db::ancestors::Ancestors;
use solana_accounts_db::accounts_db::AccountShrinkThreshold;
use solana_accounts_db::u64_align;
use solana_sdk::rent_collector::RentCollector;
use nix::sys::signal::{self, Signal};
use nix::unistd::{fork, ForkResult, Pid};

use std::fs::File;
use std::io::Write;
use std::fmt::Result;
use std::time::Duration;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryLocation {
    Ram,
    Disk
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyndicaBenchmark { 
    pub n_accounts: usize,
    pub slot_list_len: usize,
    pub accounts: MemoryLocation,
    pub index: MemoryLocation,
    pub n_accounts_multiple: usize,
}

pub fn run_accounts_disk_benchmark(benchmark: SyndicaBenchmark) -> anyhow::Result<()> {
    println!("benchmark {:?}", benchmark);
    let mut config = ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
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
            let account = AccountSharedData::new(
                10, 
                i % 1_000, 
                AccountSharedData::default().owner()
            );

            let account_inner = &[(&pubkeys[i % n_accounts], &account)][..];
            let accounts = (s as u64, account_inner);
            storage.accounts.append_accounts(
                &accounts,
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



    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {
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


        let mut slots = vec![];
        for slot in 0..slots_used {
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
    }
    Ok(ForkResult::Child) => {
        println!("Child process: running perf stat");
        
        let parent_pid = nix::unistd::getppid();
        let perf_command = format!("perf stat -p {}", parent_pid);
        
        let output = Command::new("sh")
            .arg("-c")
            .arg(&perf_command)
            .output().unwrap();

        let mut file = File::create("perf_output.txt").unwrap();
        file.write_all(&output.stderr).unwrap();
    }
    Err(err) => println!("Fork failed: {}", err),
}

    return Ok(());
}


pub fn run_accounts_ram_benchmark(benchmark: SyndicaBenchmark) -> anyhow::Result<()> {
    // println!("benchmark {:?}", benchmark);
    let mut config = ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
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
 
    let mut accounts = vec![];
    let mut refs = vec![];

    for i in 0..n_accounts { 
        let account = AccountSharedData::new(
            10, 
            i % 1000, 
            AccountSharedData::default().owner()
        );
        accounts.push(account);
    }

    // initial account amounts 
    accounts_db.add_root(0);

    let slot = 0;

    // the rest
    for i in 0..n_accounts {
        refs.push((&pubkeys[i], &accounts[i]));
    }
    
    // match unsafe { fork() } {
    //     Ok(ForkResult::Parent { child }) => {
    let timer = std::time::Instant::now();

    accounts_db.store_for_tests(slot, &refs[..]);
    let elapsed = timer.elapsed();
    println!("WRITE elapsed: {}", elapsed.as_nanos());

    let ancestors = Ancestors::from(vec![slot]);
    let t2 = std::time::Instant::now();

    for i in 0..n_accounts {
        let (account, _) = accounts_db.load_without_fixed_root(&ancestors, &pubkeys[i]).unwrap();
        assert_eq!(account.data().len(), i % 1_000);
    }

    let read_elapsed = t2.elapsed();
    println!("READ elapsed: {}", read_elapsed.as_nanos());
    // }
    // Ok(ForkResult::Child) => {
    //     println!("Child process: running perf stat");
        
    //     let parent_pid = nix::unistd::getppid();
    //     let perf_command = format!("perf stat -p {}", parent_pid);
        
    //     let output = Command::new("sh")
    //         .arg("-c")
    //         .arg(&perf_command)
    //         .output().unwrap();

    //     let mut file = File::create(format!("perf_{}_accounts_{:?}_index_{:?}.txt", benchmark.n_accounts, benchmark.accounts, benchmark.index)).unwrap();
    //     file.write_all(&output.stderr).unwrap();
    // }
    // Err(err) => println!("Fork failed: {}", err),
    // }

    return Ok(());
}