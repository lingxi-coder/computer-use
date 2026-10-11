//! Safe creation and identity pinning for one owned-input journal per private
//! device session. The file descriptor, never its pathname, reaches the helper.

use std::fs::File;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
use rustix::process::getuid;

const JOURNAL_BASENAME: &str = "owned-input-journal.jsonl";

pub(crate) struct OwnedJournalBootstrap {
    directory: File,
    file: File,
    device: u64,
    inode: u64,
}

impl OwnedJournalBootstrap {
    pub(crate) fn create(
        socket_dir: &Path,
        epoch: u64,
        registration: &OwnedDocumentRegistration,
    ) -> Result<Self, String> {
        if epoch == 0 {
            return Err("owned journal requires a live session epoch".into());
        }
        registration.validate_identity()?;
        let before = std::fs::symlink_metadata(socket_dir)
            .map_err(|_| "owned journal directory is unavailable")?;
        if !before.is_dir()
            || before.permissions().mode() & 0o777 != 0o700
            || before.uid() != getuid().as_raw()
        {
            return Err("owned journal directory is not private to this user".into());
        }
        let directory = File::from(
            open(
                socket_dir,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|_| "owned journal directory could not be pinned")?,
        );
        let pinned = directory
            .metadata()
            .map_err(|_| "owned journal directory identity is unavailable")?;
        if pinned.dev() != before.dev()
            || pinned.ino() != before.ino()
            || pinned.uid() != getuid().as_raw()
            || pinned.permissions().mode() & 0o777 != 0o700
        {
            return Err("owned journal directory changed while opening".into());
        }
        let raw_directory =
            fstat(&directory).map_err(|_| "owned journal directory identity is unavailable")?;
        if !FileType::from_raw_mode(raw_directory.st_mode).is_dir()
            || raw_directory.st_uid != getuid().as_raw()
            || raw_directory.st_mode & 0o777 != 0o700
        {
            return Err("owned journal directory handle is not private".into());
        }

        // O_EXCL makes every invocation start with an empty append chain. A
        // repeated or stale session fails closed instead of reopening a log.
        let file = File::from(
            openat(
                &directory,
                JOURNAL_BASENAME,
                OFlags::WRONLY
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::CLOEXEC
                    | OFlags::APPEND,
                Mode::RUSR | Mode::WUSR,
            )
            .map_err(|_| "owned input journal could not be created exclusively")?,
        );
        let metadata = file
            .metadata()
            .map_err(|_| "owned input journal identity is unavailable")?;
        if !metadata.is_file()
            || metadata.uid() != getuid().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() != 0
        {
            return Err("owned input journal file is not empty and private".into());
        }
        let device = metadata.dev();
        let inode = metadata.ino();
        let this = Self {
            directory,
            file,
            device,
            inode,
        };
        this.verify_empty_path()?;
        Ok(this)
    }

    pub(crate) fn clone_file(&self) -> Result<File, String> {
        self.verify_empty_path()?;
        self.file
            .try_clone()
            .map_err(|_| "owned input journal descriptor could not be inherited".into())
    }

    pub(crate) fn verify_empty_path(&self) -> Result<(), String> {
        let metadata = self
            .file
            .metadata()
            .map_err(|_| "owned input journal descriptor is unavailable")?;
        let entry = statat(&self.directory, JOURNAL_BASENAME, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|_| "owned input journal path changed")?;
        if metadata.dev() != self.device
            || metadata.ino() != self.inode
            || !metadata.is_file()
            || metadata.uid() != getuid().as_raw()
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.nlink() != 1
            || metadata.len() != 0
            || !FileType::from_raw_mode(entry.st_mode).is_file()
            || u64::try_from(entry.st_dev).ok() != Some(self.device)
            || entry.st_ino != self.inode
            || entry.st_uid != getuid().as_raw()
            || entry.st_mode & 0o777 != 0o600
            || entry.st_nlink != 1
        {
            return Err("owned input journal inode or mode changed".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn registration()
    -> interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration {
        interaction_contracts::desktop::owned_registration::OwnedDocumentRegistration {
            version: 1,
            registration_id: "123e4567-e89b-42d3-a456-426614174000".into(),
            pid: 1234,
            start_seconds: 1_700_000_000,
            start_micros: 123_456,
            executable_path: "/System/Applications/TextEdit.app/Contents/MacOS/TextEdit".into(),
            bundle_id: "com.apple.TextEdit".into(),
            document_path: "/private/tmp/owned-journal-test/document.txt".into(),
            document_sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                .into(),
            document_dev: 1,
            document_ino: 2,
            directory_dev: 1,
            directory_ino: 3,
            issued_unix_ms: 1_800_000_000_000,
        }
    }

    fn private_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn creates_one_private_empty_journal_and_refuses_a_second_session_file() {
        let directory = private_tempdir();
        let registration = registration();
        let journal = OwnedJournalBootstrap::create(directory.path(), 9, &registration).unwrap();
        let path = directory.path().join(JOURNAL_BASENAME);
        let metadata = std::fs::symlink_metadata(&path).unwrap();
        assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.len(), 0);
        journal.verify_empty_path().unwrap();
        let inherited = journal.clone_file().unwrap();
        assert_eq!(inherited.metadata().unwrap().ino(), metadata.ino());
        assert!(OwnedJournalBootstrap::create(directory.path(), 9, &registration).is_err());
    }

    #[test]
    fn refuses_public_directory_and_journal_path_replacement() {
        let public_directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            public_directory.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        assert!(
            OwnedJournalBootstrap::create(public_directory.path(), 9, &registration()).is_err()
        );

        let directory = private_tempdir();
        let journal = OwnedJournalBootstrap::create(directory.path(), 9, &registration()).unwrap();
        let path = directory.path().join(JOURNAL_BASENAME);
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(&path).unwrap();
        symlink(outside.path(), &path).unwrap();
        assert!(journal.verify_empty_path().is_err());
    }
}
