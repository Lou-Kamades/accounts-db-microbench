
use solana_accounts_db::accounts_db::ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
use solana_sdk::account::ReadableAccount; 
use solana_sdk::account::AccountSharedData;
use solana_sdk::genesis_config::ClusterType;
use solana_sdk::pubkey::Pubkey;
use solana_accounts_db::accounts_db::AccountsDb;
use solana_accounts_db::accounts_index::IndexLimitMb;
use solana_accounts_db::ancestors::Ancestors;
use solana_accounts_db::accounts_db::AccountShrinkThreshold;
use rand::distributions::{Distribution, Uniform};
use rand::Rng;
use rand::seq::SliceRandom;
use nix::sys::signal::{self, Signal};
use nix::unistd::{fork, ForkResult, Pid};

use std::fs::File;
use std::io::Write;
use std::fmt::Result;
use std::sync::Arc;
use std::time::Duration;
use std::process::Command;
use clap::{Parser, ValueEnum};
use crate::old::MemoryLocation;
use crate::old::SyndicaBenchmark;

pub mod old;

#[derive(Clone)]
enum Action {
    Read(Pubkey),
    Write((u64, Pubkey, AccountSharedData))
}

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Benchmark {

    #[arg(long, short)]
    num_slots: usize,

    #[arg(long, short)]
    init_accounts_per_slot: usize,

    #[arg(long, short)]
    /// Number of account writes during the benchmark
    accounts_to_write: usize,

    #[arg(long, short)]
    /// Ratio of reads to writes during the benchmark. Should be between 0 and 1
    read_write_ratio: f64,

    #[arg(long, short)]
    /// How many times the read/write loop should be run
    benchmark_runs: usize,
}

pub fn main() -> Result {
    // let benchmark = Benchmark::parse();
    // println!("Benchmark configuration: {:?}", benchmark);
    // run_benchmark(benchmark)?;

    let variety = vec![
        // SyndicaBenchmark { 
        //     n_accounts: 10_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Ram,
        //     index: MemoryLocation::Ram,
        //     n_accounts_multiple: 0,
        // },
        // SyndicaBenchmark { 
        //     n_accounts: 10_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Ram,
        //     index: MemoryLocation::Disk,
        //     n_accounts_multiple: 0,
        // },
        SyndicaBenchmark { 
            n_accounts: 100_000,
            slot_list_len: 1,
            accounts: MemoryLocation::Ram,
            index: MemoryLocation::Ram,
            n_accounts_multiple: 0,
        },
        // SyndicaBenchmark { 
        //     n_accounts: 100_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Ram,
        //     index: MemoryLocation::Disk,
        //     n_accounts_multiple: 0,
        // },
        // SyndicaBenchmark { 
        //     n_accounts: 1_000_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Ram,
        //     index: MemoryLocation::Ram,
        //     n_accounts_multiple: 0,
        // },
        // SyndicaBenchmark { 
        //     n_accounts: 1_000_000,
        //     slot_list_len: 1,
        //     accounts: MemoryLocation::Ram,
        //     index: MemoryLocation::Disk,
        //     n_accounts_multiple: 0,
        // },
    ];

    for benchmark in variety {
        for _ in 0..20 {
            match benchmark.accounts {
                MemoryLocation::Ram => old::run_accounts_ram_benchmark(benchmark).unwrap(),
                MemoryLocation::Disk => old::run_accounts_disk_benchmark(benchmark).unwrap(),
            }
        }
        ;
        println!("---");
    }
    return Ok(());
}

pub fn run_benchmark(benchmark: Benchmark) -> Result {
    let total_timer = std::time::Instant::now();
    let mut config = ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
    config.index.as_mut().unwrap().index_limit_mb = IndexLimitMb::InMemOnly;

    let num_slots = benchmark.num_slots;
    let init_accounts_per_slot = benchmark.init_accounts_per_slot;

    let accounts_db = AccountsDb::new_with_config(
        vec![], 
        &ClusterType::Development, 
        solana_accounts_db::accounts_index::AccountSecondaryIndexes::default(),
        AccountShrinkThreshold::default(),
        Some(config),
        None,
        Arc::default(),
    );
    accounts_db.add_root(0);

    // init accounts + keys
    let mut pubkeys = vec![];
    let mut account_datas = vec![];
    let mut rng = rand::thread_rng();
    let between = Uniform::from(136..100_000);
    let mut total_size = 0;
    for _ in 0..num_slots {
        for _ in 0..init_accounts_per_slot { 
            let account_size = between.sample(&mut rng);
            let pubkey = solana_sdk::pubkey::new_rand();
            pubkeys.push(pubkey);
            account_datas.push(
                AccountSharedData::new(
                    10, 
                    account_size, 
                    AccountSharedData::default().owner()
                )
            );
        }
    }

    // insert initial accounts
    for s in 0..num_slots {
        let mut accounts = vec![];
        for i in 0..init_accounts_per_slot { 
            let index = (s * init_accounts_per_slot) + i;
            let account = (&pubkeys[index], &account_datas[index]);
            accounts.push(account);
        }
        accounts_db.store_for_tests(s as u64, &accounts[..]);
    }

    // generate read/write actions for benchmark
    let mut slots = vec![];
    for slot in 0..num_slots {
        slots.push(slot as u64); 
    }
    let ancestors = Ancestors::from(slots);
    let mut accounts_to_write = benchmark.accounts_to_write;
    let mut actions = vec![];
    let mut num_reads = 0;
    let mut num_writes = 0;
    while accounts_to_write > 0 {
        let random_number: f64 = rng.gen();
        let key_index = rng.gen_range(0..pubkeys.len());
        let write_slot = rng.gen_range(0..num_slots);
        if random_number > benchmark.read_write_ratio {
            num_writes += 1;
            accounts_to_write -= 1;
            actions.push(Action::Write((write_slot as u64, pubkeys[key_index], account_datas[key_index].clone())))
        } else {
            num_reads += 1;
            actions.push(Action::Read(pubkeys[key_index]))
        };
    }

    // TODO: average etc
    for i in 0..benchmark.benchmark_runs {
        let mut trial_actions = actions.clone();
        trial_actions.shuffle(&mut rng);
        let elapsed = run_benchmark_inner(&accounts_db, trial_actions, &ancestors);
        println!("Elapsed: {}, reads: {}, writes: {} ", elapsed.as_secs_f64(), num_reads, num_writes);
    }
    
    let total_elapsed = total_timer.elapsed();
    println!("Total time: {}", total_elapsed.as_secs_f64());

    return Ok(());
}

#[inline(never)]
fn run_benchmark_inner(accounts_db: &AccountsDb, actions: Vec<Action>, ancestors: &Ancestors) -> Duration {
    let timer = std::time::Instant::now();
    match unsafe { fork() } {
        Ok(ForkResult::Parent { child }) => {

            println!("Parent process: child pid is {}", child);
            // Main program 
            for action in actions {
                match action {
                    Action::Read(key) => {
                        let (account, _) = accounts_db.load_without_fixed_root(ancestors, &key).unwrap();
                        assert!(account.data().len() != 0);
                    },
                    Action::Write((s, key, data)) => {
                        accounts_db.store_for_tests(s, &[(&key, &data)]);
                        
                    },
                }
            }

            // Signal the child to finish
            signal::kill(child, Signal::SIGTERM).unwrap();

            // Wait for child to finish
            nix::sys::wait::waitpid(child, None).unwrap();
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
    timer.elapsed()
}

