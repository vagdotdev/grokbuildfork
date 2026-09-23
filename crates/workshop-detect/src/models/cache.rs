//! The last good model list per rail, in Workshop's own cache directory
//! (`$WORKSHOP_HOME/catalog-cache/<rail>-models.json`). The only files this crate reads or writes.

use std::path::PathBuf;

use super::SubscriptionModels;
use crate::model::Rail;

#[derive(Debug, Clone)]
pub struct ModelsCache {
    dir: PathBuf,
}

impl ModelsCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn path(&self, rail: Rail) -> PathBuf {
        self.dir.join(format!("{}-models.json", rail.vendor().id()))
    }

    /// The stored list for `rail`, if it parses and is a real (not alias) list for that rail.
    pub fn load(&self, rail: Rail) -> Option<SubscriptionModels> {
        let text = std::fs::read_to_string(self.path(rail)).ok()?;
        serde_json::from_str::<SubscriptionModels>(&text)
            .ok()
            .filter(|l| l.rail == rail && !l.documented_aliases)
    }

    /// Replace the stored list (owner-only file, written to a temp name and renamed).
    pub fn store(&self, list: &SubscriptionModels) {
        let Ok(json) = serde_json::to_vec_pretty(list) else {
            return;
        };
        let path = self.path(list.rail);
        let tmp = path.with_extension(format!("json.tmp-{}", std::process::id()));
        if std::fs::create_dir_all(&self.dir).is_err() {
            return;
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let written = opts.open(&tmp).and_then(|mut f| {
            use std::io::Write;
            f.write_all(&json)?;
            f.sync_all()
        });
        if written.is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }

    pub fn forget(&self, rail: Rail) {
        let _ = std::fs::remove_file(self.path(rail));
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Account, SubscriptionModel};
    use super::*;

    fn list(rail: Rail) -> SubscriptionModels {
        SubscriptionModels {
            rail,
            models: vec![SubscriptionModel {
                id: "sonnet".into(),
                label: "Sonnet".into(),
                is_default: false,
            }],
            account: Some(Account {
                email: Some("user@example.com".into()),
                plan: Some("max".into()),
            }),
            documented_aliases: false,
            fetched_at_secs: 1_790_000_000,
        }
    }

    #[test]
    fn store_load_forget_round_trip_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ModelsCache::new(tmp.path().join("catalog-cache"));
        assert_eq!(cache.load(Rail::Claude), None);
        cache.store(&list(Rail::Claude));
        assert_eq!(cache.load(Rail::Claude), Some(list(Rail::Claude)));
        assert!(
            cache
                .path(Rail::Claude)
                .ends_with("catalog-cache/claude-models.json")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(cache.path(Rail::Claude))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(cache.load(Rail::Codex), None);
        cache.forget(Rail::Claude);
        assert_eq!(cache.load(Rail::Claude), None);
    }

    #[test]
    fn a_file_for_another_rail_or_an_alias_list_is_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = ModelsCache::new(tmp.path());
        std::fs::write(
            cache.path(Rail::Codex),
            serde_json::to_vec(&list(Rail::Claude)).unwrap(),
        )
        .unwrap();
        assert_eq!(cache.load(Rail::Codex), None);
        let mut aliases = list(Rail::Claude);
        aliases.documented_aliases = true;
        std::fs::write(
            cache.path(Rail::Claude),
            serde_json::to_vec(&aliases).unwrap(),
        )
        .unwrap();
        assert_eq!(cache.load(Rail::Claude), None);
        std::fs::write(cache.path(Rail::Cursor), "{not json").unwrap();
        assert_eq!(cache.load(Rail::Cursor), None);
    }
}
