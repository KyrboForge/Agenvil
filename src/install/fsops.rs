#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use std::io;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn probe() -> io::Result<()> {
    #[cfg(windows)]
    {
        use crate::stage::pinned::Root;
        use std::{env, ffi::OsStr, fs, os::windows::fs::symlink_file, path::Path};

        let temp = std::path::absolute(env::temp_dir())?;
        let _parent = Root::open_checked(&temp)?.ok_or(io::ErrorKind::NotFound)?;
        let dir = temp.join(temp_name("symlink-probe"));
        fs::create_dir(&dir)?;
        let root = Root::open_checked(&dir)?.ok_or(io::ErrorKind::NotFound)?;
        let name = temp_name("link");
        let link = dir.join(&name);
        let target = Path::new("missing");
        symlink_file(target, &link)?;
        if root.link(OsStr::new(&name))? != target {
            return Err(io::Error::other("temporary symlink probe changed"));
        }
        fs::remove_file(link)?;
        drop(root);
        fs::remove_dir(dir)?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos", windows))]
pub(super) fn temp_name(purpose: &str) -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        ".tmp-agenvil-{purpose}-{}-{time}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use std::ffi::OsStr;
    use std::fs::{File, TryLockError};
    use std::io::{self, Write};
    use std::path::{Component, Path};

    use rustix::fs::{self, Mode, OFlags, RenameFlags};
    use rustix::io::Errno;

    use super::temp_name;
    use crate::install::{
        Key,
        actual::{SnapshotCheck, inspect_snapshot, node_identity, observe, stat_identity},
        desired::Resource,
        ledger::{self, Quarantine},
        plan::Observed,
        same_target,
    };
    use crate::{manifest, stage::pinned::Root};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(in crate::install) enum Point {
        Directory,
        Lock,
        Snapshot,
        Journal,
        Commit,
        RestoreLedger,
        LedgerSync,
        LedgerCompared,
        Create,
        Replace,
        Remove,
        Rollback,
        Quarantined,
        RetireVerified,
    }

    pub(in crate::install) struct Fs<'a>(pub &'a mut dyn FnMut(Point) -> io::Result<()>);

    pub(in crate::install) struct PersistError {
        pub source: io::Error,
        pub published: bool,
        pub disputed: bool,
    }

    impl From<io::Error> for PersistError {
        fn from(source: io::Error) -> Self {
            Self {
                source,
                published: false,
                disputed: false,
            }
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("{cause}; inspect {quarantine} next to {leaf}")]
    pub(in crate::install) struct QuarantineError {
        cause: io::Error,
        quarantine: String,
        leaf: String,
    }

    impl Fs<'_> {
        pub fn path(&mut self, path: &Path) -> io::Result<Root> {
            let mut dir = Root::open_checked(Path::new("/"))?.expect("filesystem root exists");
            for part in path.components() {
                if let Component::Normal(name) = part {
                    dir = self.dir(&dir, name)?;
                }
            }
            Ok(dir)
        }

        pub fn dir(&mut self, parent: &Root, name: &OsStr) -> io::Result<Root> {
            if let Some(directory) = parent.child(name)? {
                return Ok(directory);
            }
            (self.0)(Point::Directory)?;
            match fs::mkdirat(parent, name, Mode::from_raw_mode(0o755)) {
                Ok(()) => fs::fsync(parent)?,
                Err(Errno::EXIST) => {}
                Err(err) => return Err(err.into()),
            }
            parent
                .child(name)?
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "directory disappeared"))
        }

        pub fn lock(&mut self, state: &Root) -> io::Result<File> {
            (self.0)(Point::Lock)?;
            let flags = OFlags::RDWR
                | OFlags::CREATE
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC;
            let file = File::from(fs::openat(
                state,
                ledger::LOCK,
                flags,
                Mode::from_raw_mode(0o600),
            )?);
            if !file.metadata()?.is_file() {
                return Err(io::Error::other("ownership.lock must be a regular file"));
            }
            file.try_lock().map_err(|err| match err {
                TryLockError::WouldBlock => io::Error::from(io::ErrorKind::WouldBlock),
                TryLockError::Error(err) => err,
            })?;
            Ok(file)
        }

        pub fn snapshot(&mut self, state: &Root, key: &Key, resource: &Resource) -> io::Result<()> {
            (self.0)(Point::Snapshot)?;
            let snapshots = self.dir(state, OsStr::new("snapshots"))?;
            let tmp = temp_name("snapshot");
            fs::mkdirat(&snapshots, &*tmp, Mode::from_raw_mode(0o700))?;
            let bucket = snapshots
                .child(OsStr::new(&tmp))?
                .ok_or(io::ErrorKind::NotFound)?;
            let leaf = key.leaf();
            let skill;
            let files = if key.kind == crate::install::Kind::Skill {
                skill = self.snapshot_dir(&bucket, &leaf)?;
                &skill
            } else {
                &bucket
            };
            for (path, content) in &resource.files {
                let mut parent = None;
                let (dirs, file) = path.rsplit_once('/').unwrap_or(("", path));
                for part in dirs.split('/').filter(|part| !part.is_empty()) {
                    parent = Some(self.snapshot_dir(parent.as_ref().unwrap_or(files), part)?);
                }
                let parent = parent.as_ref().unwrap_or(files);
                let mode = if content.exec { 0o555 } else { 0o444 };
                write_file(parent, file, &content.bytes, mode)?;
                fs::fsync(parent)?;
            }
            fs::fchmod(&bucket, Mode::from_raw_mode(0o755))?;
            fs::fsync(&bucket)?;
            match rename_new(&snapshots, &tmp, &resource.digest) {
                Ok(()) => fs::fsync(&snapshots)?,
                Err(err)
                    if err.kind() == io::ErrorKind::AlreadyExists
                        && inspect_snapshot(Some(state), key, &resource.digest)?
                            == SnapshotCheck::Verified => {}
                Err(err) => return Err(err),
            }
            Ok(())
        }

        fn snapshot_dir(&mut self, parent: &Root, name: &str) -> io::Result<Root> {
            let directory = self.dir(parent, OsStr::new(name))?;
            fs::fchmod(&directory, Mode::from_raw_mode(0o755))?;
            fs::fsync(&directory)?;
            Ok(directory)
        }

        pub fn persist(
            &mut self,
            state: &Root,
            expected: Option<&str>,
            bytes: Option<&[u8]>,
            point: Point,
        ) -> Result<(), PersistError> {
            (self.0)(point)?;
            let current = ledger::read_bytes(Some(state))?;
            if current.as_deref().map(manifest::sha256).as_deref() != expected {
                return Err(ledger_changed().into());
            }
            (self.0)(Point::LedgerCompared)?;
            let temp = temp_name("ledger");
            if let Some(bytes) = bytes {
                write_file(state, &temp, bytes, 0o600)?;
            }
            let replacement = match (expected, bytes) {
                (None, None) => return Ok(()),
                (None, Some(_)) => {
                    rename_new(state, &temp, ledger::FILE)?;
                    false
                }
                (Some(_), Some(_)) => {
                    rename_with(state, &temp, ledger::FILE, RenameFlags::EXCHANGE)?;
                    true
                }
                (Some(_), None) => {
                    rename_new(state, ledger::FILE, &temp)?;
                    true
                }
            };
            if replacement {
                let displaced = named_digest(state, &temp);
                if !matches!(&displaced, Ok(digest) if digest.as_deref() == expected) {
                    let restore = match bytes {
                        Some(bytes)
                            if matches!(named_digest(state, ledger::FILE), Ok(Some(digest))
                                if digest == manifest::sha256(bytes)) =>
                        {
                            rename_with(state, &temp, ledger::FILE, RenameFlags::EXCHANGE)
                        }
                        Some(_) => Err(ledger_changed()),
                        None => rename_new(state, &temp, ledger::FILE),
                    };
                    return match restore.and_then(|()| Ok(fs::fsync(state)?)) {
                        Ok(()) => Err(displaced.err().unwrap_or_else(ledger_changed).into()),
                        Err(err) => Err(PersistError {
                            source: io::Error::other(format!(
                                "ownership changed during publication; restoration failed ({err}); inspect {temp}"
                            )),
                            published: false,
                            disputed: true,
                        }),
                    };
                }
            }
            // Neither exchange nor no-replace can safely unlink a displaced name.
            (self.0)(Point::LedgerSync)
                .and_then(|()| Ok(fs::fsync(state)?))
                .map_err(|source| PersistError {
                    source,
                    published: true,
                    disputed: false,
                })?;
            let visible = ledger::read_bytes(Some(state));
            if !matches!(&visible, Ok(current) if current.as_deref().map(manifest::sha256)
                == bytes.map(manifest::sha256))
            {
                return Err(PersistError {
                    source: io::Error::other(format!(
                        "ownership changed after publication; inspect {temp} and {}",
                        ledger::FILE
                    )),
                    published: false,
                    disputed: true,
                });
            }
            Ok(())
        }

        pub fn link(
            &mut self,
            parent: &Root,
            leaf: &str,
            target: &Path,
            point: Point,
        ) -> io::Result<()> {
            (self.0)(point)?;
            Ok(fs::symlinkat(target, parent, leaf)?)
        }

        pub fn retire(
            &mut self,
            parent: &Root,
            leaf: &str,
            item: &Quarantine,
            point: Point,
        ) -> io::Result<()> {
            #[cfg(target_os = "linux")]
            let flags = OFlags::PATH | OFlags::NOFOLLOW | OFlags::CLOEXEC;
            #[cfg(target_os = "macos")]
            let flags = OFlags::SYMLINK | OFlags::NONBLOCK | OFlags::CLOEXEC;
            let pinned = fs::openat(parent, leaf, flags, Mode::empty())?;
            let stat = fs::fstat(&pinned)?;
            let id = stat_identity(&stat);
            if fs::FileType::from_raw_mode(stat.st_mode) != fs::FileType::Symlink
                || item.identity.is_some_and(|expected| id != expected)
                || !matches!(observe(Some(parent), leaf)?, Observed::Symlink { target }
                    if same_target(&item.target, &target))
                || node_identity(parent, leaf)? != Some(id)
            {
                return Err(io::Error::other("destination changed before retirement"));
            }

            (self.0)(point)?;
            rename_new(parent, leaf, &item.name)?;
            let retained = |cause| {
                io::Error::other(QuarantineError {
                    cause,
                    quarantine: item.name.clone(),
                    leaf: leaf.to_owned(),
                })
            };
            (self.0)(Point::Quarantined).map_err(retained)?;
            let verify = || -> io::Result<()> {
                if node_identity(parent, &item.name)? == Some(id)
                    && matches!(observe(Some(parent), &item.name)?, Observed::Symlink { target }
                        if same_target(&item.target, &target))
                    && node_identity(parent, &item.name)? == Some(id)
                {
                    Ok(())
                } else {
                    Err(io::Error::other("destination changed during retirement"))
                }
            };
            if let Err(err) = verify() {
                return match (self.0)(Point::Rollback)
                    .and_then(|()| rename_new(parent, &item.name, leaf))
                {
                    Ok(()) => Err(err),
                    Err(restore) => Err(retained(io::Error::other(format!(
                        "{err}; restore failed ({restore})"
                    )))),
                };
            }
            (self.0)(Point::RetireVerified).map_err(retained)?;
            // Keep the verified link as well. Unlinking by name would reopen the race.
            let result = verify().map_err(retained);
            drop(pinned);
            result
        }

        pub fn restore(&mut self, parent: &Root, leaf: &str, item: &Quarantine) -> io::Result<()> {
            (self.0)(Point::Rollback)?;
            if item.identity.is_none()
                || node_identity(parent, &item.name)? != item.identity
                || !matches!(observe(Some(parent), &item.name)?, Observed::Symlink { target }
                    if same_target(&item.target, &target))
            {
                return Err(io::Error::other("retired link changed before restoration"));
            }
            rename_new(parent, &item.name, leaf)
        }

        pub fn sync(&self, dir: &Root) -> io::Result<()> {
            Ok(fs::fsync(dir)?)
        }
    }

    fn write_file(parent: &Root, name: &str, bytes: &[u8], mode: u32) -> io::Result<()> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let mut file = File::from(fs::openat(parent, name, flags, Mode::from_raw_mode(0o600))?);
        file.write_all(bytes)?;
        fs::fchmod(&file, Mode::from_raw_mode(mode as _))?;
        file.sync_all()
    }

    fn ledger_changed() -> io::Error {
        io::Error::other("ownership ledger changed; re-plan")
    }

    fn named_digest(dir: &Root, name: &str) -> io::Result<Option<String>> {
        Ok(dir
            .read(name, manifest::MAX_BYTES + 1)?
            .as_deref()
            .map(manifest::sha256))
    }

    fn rename_new(dir: &Root, from: &str, to: &str) -> io::Result<()> {
        rename_with(dir, from, to, RenameFlags::NOREPLACE)
    }

    fn rename_with(dir: &Root, from: &str, to: &str, flags: RenameFlags) -> io::Result<()> {
        match fs::renameat_with(dir, from, dir, to, flags) {
            Ok(()) => Ok(()),
            Err(Errno::NOSYS | Errno::OPNOTSUPP | Errno::INVAL) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("filesystem does not support atomic rename with {flags:?}"),
            )),
            Err(err) => Err(err.into()),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) use native::{Fs, Point, QuarantineError};
