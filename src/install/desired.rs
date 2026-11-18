use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::{Key, Kind, PlanError, ledger::Applied};
use crate::{build, config::Config, inventory::Inventory, manifest, stage};

pub(super) struct Desired {
    pub resources: BTreeMap<Key, Resource>,
    pub applied: Applied,
}

pub(super) struct Resource {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub files: BTreeMap<String, Content>,
    pub digest: String,
}

pub(super) struct Content {
    pub bytes: Vec<u8>,
    pub exec: bool,
}

impl Desired {
    pub fn load(root: &Path, config: &Config, out: &Path) -> Result<Self, PlanError> {
        let inventory = Inventory::build(root, config, &build::limits())?;
        let entries = build::expected(root, config, &inventory)?;
        let (generator, manifest_sha256) = stage::approved_manifest(root, config, out, &entries)?
            .map_err(|findings| PlanError::Staging {
            out: out.to_owned(),
            findings,
        })?;

        let mut grouped = BTreeMap::<Key, BTreeMap<String, Content>>::new();
        for entry in entries {
            if entry.path == manifest::INSTRUCTIONS {
                continue;
            }
            let (key, rel) = resource_path(&entry.path)
                .ok_or_else(|| PlanError::Resource(entry.path.clone()))?;
            grouped.entry(key).or_default().insert(
                rel.to_owned(),
                Content {
                    bytes: entry.bytes,
                    exec: entry.exec,
                },
            );
        }
        let resources = grouped
            .into_iter()
            .map(|(key, files)| {
                let digest = resource_digest(
                    &key,
                    files.iter().map(|(name, file)| {
                        (name.as_str(), manifest::sha256(&file.bytes), file.exec)
                    }),
                );
                (
                    key,
                    Resource {
                        #[cfg(any(target_os = "linux", target_os = "macos"))]
                        files,
                        digest,
                    },
                )
            })
            .collect();
        Ok(Self {
            resources,
            applied: Applied {
                kit: config.project.name.clone(),
                manifest_sha256,
                generator,
            },
        })
    }
}

fn resource_path(path: &str) -> Option<(Key, &str)> {
    let (directory, resource) = path.strip_prefix(".claude/")?.split_once('/')?;
    let kind = Kind::from_directory(directory)?;
    let (leaf, file) = match kind {
        Kind::Skill => resource.split_once('/')?,
        Kind::Agent | Kind::Rule => (resource, resource),
    };
    Some((Key::from_filename(kind, leaf)?, file))
}

pub(super) fn resource_digest(
    key: &Key,
    files: impl IntoIterator<Item = (impl AsRef<str>, String, bool)>,
) -> String {
    let mut hash = Sha256::new();
    hash.update(key.kind.directory());
    hash.update([0]);
    hash.update(&key.name);
    hash.update([0]);
    for (relative, digest, exec) in files {
        hash.update(relative.as_ref());
        hash.update([0]);
        hash.update(digest);
        hash.update([u8::from(exec)]);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
