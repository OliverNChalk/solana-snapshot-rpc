use {
    crate::{
        append_vec::AppendVec,
        solana::{
            deserialize_from, AccountsDbFields, DeserializableVersionedBank,
            SerializableAccountStorageEntry,
        },
        utils::{parse_append_vec_name, ReadProgressTracking, SnapshotError, SnapshotResult},
    },
    itertools::Itertools,
    solana_runtime::snapshot_utils::SNAPSHOT_STATUS_CACHE_FILENAME,
    std::{
        fs::OpenOptions,
        io::BufReader,
        path::{Path, PathBuf},
        str::FromStr,
        time::Instant,
    },
    tracing::info,
};

/// Extracts account data from snapshots that were unarchived to a file system.
pub(crate) struct UnpackedSnapshotExtractor {
    root: PathBuf,
    accounts_db_fields: AccountsDbFields<SerializableAccountStorageEntry>,
}

impl UnpackedSnapshotExtractor {
    pub(crate) fn open(
        path: &Path,
        progress_tracking: Box<dyn ReadProgressTracking>,
    ) -> SnapshotResult<Self> {
        let snapshots_dir = path.join("snapshots");
        let status_cache = snapshots_dir.join(SNAPSHOT_STATUS_CACHE_FILENAME);
        if !status_cache.is_file() {
            return Err(SnapshotError::NoStatusCache);
        }

        let snapshot_files = snapshots_dir.read_dir()?;

        let snapshot_file_path = snapshot_files
            .take(10)
            .filter_map(|entry| entry.ok())
            .find(|entry| u64::from_str(&entry.file_name().to_string_lossy()).is_ok())
            .map(|entry| entry.path().join(entry.file_name()))
            .ok_or(SnapshotError::NoSnapshotManifest)?;

        info!("Opening snapshot manifest: {:?}", snapshot_file_path);
        let snapshot_file = OpenOptions::new().read(true).open(&snapshot_file_path)?;
        let snapshot_file_len = snapshot_file.metadata()?.len();

        let snapshot_file = progress_tracking.new_read_progress_tracker(
            &snapshot_file_path,
            Box::new(snapshot_file),
            snapshot_file_len,
        )?;
        let mut snapshot_file = BufReader::new(snapshot_file);

        let pre_unpack = Instant::now();
        let versioned_bank: DeserializableVersionedBank = deserialize_from(&mut snapshot_file)?;
        drop(versioned_bank);
        let versioned_bank_post_time = Instant::now();

        let accounts_db_fields: AccountsDbFields<SerializableAccountStorageEntry> =
            deserialize_from(&mut snapshot_file)?;
        let accounts_db_fields_post_time = Instant::now();
        drop(snapshot_file);

        info!(
            "Read bank fields in {:?}",
            versioned_bank_post_time - pre_unpack
        );
        info!(
            "Read accounts DB fields in {:?}",
            accounts_db_fields_post_time - versioned_bank_post_time
        );

        Ok(UnpackedSnapshotExtractor {
            root: path.to_path_buf(),
            accounts_db_fields,
        })
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn unboxed_iter(&self) -> impl Iterator<Item = SnapshotResult<AppendVec>> + '_ {
        std::iter::once(self.iter_streams())
            .flatten_ok()
            .flatten_ok()
    }

    fn iter_streams(&self) -> SnapshotResult<impl Iterator<Item = SnapshotResult<AppendVec>> + '_> {
        let accounts_dir = self.root.join("accounts");
        Ok(accounts_dir.read_dir().unwrap().map(move |file| {
            let file = file.unwrap();
            let name = file.file_name();

            let (slot, version) = parse_append_vec_name(&name).unwrap();

            Ok(self
                .open_append_vec(slot, version, &accounts_dir.join(&name))
                .unwrap())
        }))
    }

    pub(crate) fn open_append_vec(
        &self,
        slot: u64,
        id: u64,
        path: &Path,
    ) -> SnapshotResult<AppendVec> {
        let known_vecs = self
            .accounts_db_fields
            .0
            .get(&slot)
            .map(|v| &v[..])
            .unwrap_or(&[]);
        let known_vec = known_vecs.iter().find(|entry| entry.id == (id as usize));
        let known_vec = match known_vec {
            None => return Err(SnapshotError::UnexpectedAppendVec),
            Some(v) => v,
        };

        Ok(AppendVec::new_from_file(
            path,
            known_vec.accounts_current_len,
            slot,
            id,
        )?)
    }
}
