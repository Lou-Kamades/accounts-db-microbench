use solana_accounts_db::account_storage::AccountStorageMap;
use solana_accounts_db::accounts_db::AtomicAccountsFileId;
use solana_accounts_db::accounts_file::StorageAccess;
use solana_accounts_db::accounts_index::AccountSecondaryIndexes;
use solana_measure::measure;
use solana_runtime::serde_snapshot::fields_from_streams;
use solana_runtime::serde_snapshot::reconstruct_accountsdb_from_fields;
use solana_runtime::snapshot_utils::deserialize_snapshot_data_files;
use solana_runtime::snapshot_utils::get_highest_bank_snapshot_post;
use solana_runtime::snapshot_utils::snapshot_storage_rebuilder::SnapshotStorageRebuilder;
use solana_runtime::snapshot_utils::streaming_snapshot_dir_files;
use solana_accounts_db::accounts_db::AccountShrinkThreshold;
use solana_accounts_db::accounts_db::AccountsDb;
use solana_accounts_db::accounts_db::ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
use solana_runtime::snapshot_utils::snapshot_storage_rebuilder::RebuiltSnapshotStorage;
use solana_runtime::snapshot_utils::BankSnapshotInfo;
use solana_runtime::snapshot_utils::SnapshotFrom;
use solana_runtime::snapshot_utils::SnapshotRootPaths;
use solana_runtime::snapshot_utils::StorageAndNextAccountsFileId;
use solana_runtime::snapshot_utils::SNAPSHOT_VERSION_FILENAME;

use std::env;
use std::fmt::Result;
use std::fs;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

pub fn main() -> Result {
    let _db = crate::load_snapshot().unwrap();
    return Ok(());
}

pub fn load_snapshot() -> anyhow::Result<AccountsDb> {
    let args: Vec<String> = env::args().collect();
    println!("{:?}", args);

    let data_dir = &args[1];
    let slot: u64 = args[2].parse().unwrap();

    let account_paths_str = &format!("{}/accounts", data_dir);
    let account_paths = vec![account_paths_str.into()];
    let snapshot_path = format!("{}/snapshots/{}", data_dir, slot);

    let version_path = format!("{}/version", snapshot_path);
    let completion_flag_path = format!("{}/state_complete", snapshot_path);
    let status_cache_path = format!("{}/status_cache", snapshot_path);

    if !fs::metadata(&completion_flag_path).is_ok() {
        fs::File::create(completion_flag_path).unwrap();
    }
    if !fs::metadata(&version_path).is_ok() {
        let mut version_file = File::create(version_path).expect("create version failed");
        version_file
            .write("1.2.0".as_bytes())
            .expect("write version failed");
    }
    if !fs::metadata(&status_cache_path).is_ok() {
        fs::File::create(status_cache_path).unwrap();
    }

    let exit = Arc::new(AtomicBool::new(false));

    let mut accounts_db_config = ACCOUNTS_DB_CONFIG_FOR_BENCHMARKS;
    accounts_db_config.base_working_path = Some(data_dir.clone().into());
    accounts_db_config.accounts_hash_cache_path = Some(
        format!(
            "{}/{}",
            data_dir,
            AccountsDb::DEFAULT_ACCOUNTS_HASH_CACHE_DIR
        )
        .into(),
    );

    // mostly taken from bank_from_snapshot_dir
    println!("{:?}", snapshot_path);
    let bank_snapshot_info = get_highest_bank_snapshot_post(format!("{}/snapshots", data_dir))
        .expect("Could not find snapshot");
    let next_append_vec_id = Arc::new(AtomicAccountsFileId::new(0));
    let storage_access = accounts_db_config.storage_access.clone();

    let (storage, measure_rebuild_storages) = measure!(
        rebuild_storages_from_snapshot_dir(
            &bank_snapshot_info,
            &account_paths,
            next_append_vec_id.clone(),
            storage_access,
        )?,
        "rebuild storages from snapshot dir"
    );
    println!("{}", measure_rebuild_storages);
    // println!("{:?}", storage);

    let next_append_vec_id =
        Arc::try_unwrap(next_append_vec_id).expect("this is the only strong reference");
    let storage_and_next_append_vec_id = StorageAndNextAccountsFileId {
        storage,
        next_append_vec_id,
    };

    let snapshot_root_paths = SnapshotRootPaths {
        full_snapshot_root_file_path: bank_snapshot_info.snapshot_path(),
        incremental_snapshot_root_file_path: None,
    };

    let ((accounts_db, _reconstructed_accounts_db_info), account_deser_measure) = measure!(
        deserialize_snapshot_data_files(&snapshot_root_paths, |snapshot_streams| {
            let (snapshot_bank_fields, snapshot_accounts_db_fields) =
                fields_from_streams(snapshot_streams)?;

            let capitalizations = (
                snapshot_bank_fields.full.capitalization,
                snapshot_bank_fields
                    .incremental
                    .as_ref()
                    .map(|bank_fields| bank_fields.capitalization),
            );
            let bank_fields = snapshot_bank_fields.collapse_into();

            let (accounts_db, reconstructed_accounts_db_info) = reconstruct_accountsdb_from_fields(
                snapshot_accounts_db_fields,
                &account_paths,
                storage_and_next_append_vec_id,
                &solana_sdk::genesis_config::GenesisConfig::default(),
                AccountSecondaryIndexes::default(),
                None, //limit_load_slot_count_from_snapshot,
                AccountShrinkThreshold::default(),
                false, // verify_index
                Some(accounts_db_config),
                None, // accounts_update_notifier
                exit,
                bank_fields.epoch_accounts_hash,
                capitalizations,
                bank_fields.incremental_snapshot_persistence.as_ref(),
            )?;

            Ok((accounts_db, reconstructed_accounts_db_info))
        })?,
        "reconstruct_accountsdb_from_fields"
    );

    println!("{}", account_deser_measure);

    Ok(accounts_db)
}

/// Performs the common tasks when deserializing a snapshot
/// Removes hardlinks operations from the original function
pub fn rebuild_storages_from_snapshot_dir(
    snapshot_info: &BankSnapshotInfo,
    account_paths: &[PathBuf],
    next_append_vec_id: Arc<AtomicAccountsFileId>,
    storage_access: StorageAccess,
) -> anyhow::Result<AccountStorageMap> {
    let bank_snapshot_dir = &snapshot_info.snapshot_dir;

    let (file_sender, file_receiver) = crossbeam_channel::unbounded();
    let snapshot_file_path = &snapshot_info.snapshot_path();
    let snapshot_version_path = bank_snapshot_dir.join(SNAPSHOT_VERSION_FILENAME);
    streaming_snapshot_dir_files(
        file_sender,
        snapshot_file_path,
        snapshot_version_path,
        account_paths,
    )?;

    let num_rebuilder_threads = num_cpus::get_physical().saturating_sub(1).max(1);

    let version_and_storages = SnapshotStorageRebuilder::rebuild_storage(
        file_receiver,
        num_rebuilder_threads,
        next_append_vec_id,
        SnapshotFrom::Dir,
        storage_access,
    )?;

    let RebuiltSnapshotStorage {
        snapshot_version: _,
        storage,
    } = version_and_storages;
    Ok(storage)
}

#[cfg(test)]
mod test {
    use crate::gbs;
    use solana_runtime::snapshot_utils::get_bank_snapshots;

    #[test]
    pub fn test_load_snapshot() -> anyhow::Result<()> {
        let _db = crate::load_snapshot()?;
        Ok(())
    }
}
