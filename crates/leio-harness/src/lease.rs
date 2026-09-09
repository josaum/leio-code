use crate::model::{AgentLease, LeaseRegistry, LeaseState};
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use fs2::FileExt;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub struct LeaseStore {
    path: PathBuf,
    lock_path: PathBuf,
}

impl LeaseStore {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let lock_path = path.with_extension("lock");
        Self { path, lock_path }
    }

    pub fn list(&self) -> Result<Vec<AgentLease>> {
        Ok(self.read()?.leases)
    }

    pub fn acquire(&self, lease: AgentLease) -> Result<AgentLease> {
        if lease.state != LeaseState::Active {
            bail!("new lease must be active");
        }
        self.mutate(|registry| {
            for active in registry.leases.iter().filter(|candidate| {
                candidate.state == LeaseState::Active && candidate.run_id != lease.run_id
            }) {
                if active.worktree_path == lease.worktree_path {
                    bail!("worktree conflict with {}", active.run_id);
                }
                if active.branch == lease.branch {
                    bail!("branch conflict with {}", active.run_id);
                }
                if lease.deployment_target.is_some()
                    && active.deployment_target == lease.deployment_target
                {
                    bail!("deployment target conflict with {}", active.run_id);
                }
                if lease.build_concurrency_group.is_some()
                    && active.build_concurrency_group == lease.build_concurrency_group
                {
                    bail!("build concurrency conflict with {}", active.run_id);
                }
                if let Some(key) = lease
                    .scope_keys
                    .iter()
                    .find(|key| active.scope_keys.contains(key))
                {
                    bail!("scope `{key}` conflict with {}", active.run_id);
                }
            }
            registry
                .leases
                .retain(|candidate| candidate.run_id != lease.run_id);
            registry.leases.push(lease.clone());
            Ok(lease)
        })
    }

    pub fn heartbeat(&self, run_id: &str, now: DateTime<Utc>) -> Result<AgentLease> {
        self.update(run_id, |lease| {
            if lease.state != LeaseState::Active {
                bail!("lease is not active");
            }
            lease.heartbeat = now.to_rfc3339();
            Ok(())
        })
    }

    pub fn release(&self, run_id: &str, now: DateTime<Utc>) -> Result<AgentLease> {
        self.update(run_id, |lease| {
            lease.state = LeaseState::Released;
            lease.heartbeat = now.to_rfc3339();
            Ok(())
        })
    }

    pub fn expire_stale(&self, now: DateTime<Utc>, stale_after_ms: i64) -> Result<Vec<AgentLease>> {
        if stale_after_ms <= 0 {
            bail!("stale_after_ms must be positive");
        }
        self.mutate(|registry| {
            let mut expired = Vec::new();
            for lease in &mut registry.leases {
                let heartbeat = DateTime::parse_from_rfc3339(&lease.heartbeat)
                    .map(|value| value.with_timezone(&Utc));
                let is_stale = heartbeat
                    .map(|value| (now - value).num_milliseconds() >= stale_after_ms)
                    .unwrap_or(true);
                if lease.state == LeaseState::Active && is_stale {
                    lease.state = LeaseState::Expired;
                    expired.push(lease.clone());
                }
            }
            expired.sort_by(|left, right| left.run_id.cmp(&right.run_id));
            Ok(expired)
        })
    }

    fn update(
        &self,
        run_id: &str,
        operation: impl FnOnce(&mut AgentLease) -> Result<()>,
    ) -> Result<AgentLease> {
        self.mutate(|registry| {
            let lease = registry
                .leases
                .iter_mut()
                .find(|lease| lease.run_id == run_id)
                .with_context(|| format!("unknown lease: {run_id}"))?;
            operation(lease)?;
            Ok(lease.clone())
        })
    }

    fn mutate<T>(&self, operation: impl FnOnce(&mut LeaseRegistry) -> Result<T>) -> Result<T> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&self.lock_path)?;
        lock.lock_exclusive()?;
        let mut registry = self.read()?;
        let output = operation(&mut registry)?;
        registry.updated_at = Utc::now().to_rfc3339();
        registry
            .leases
            .sort_by(|left, right| left.run_id.cmp(&right.run_id));
        self.write_atomic(&registry)?;
        lock.unlock()?;
        Ok(output)
    }

    fn read(&self) -> Result<LeaseRegistry> {
        if !self.path.exists() {
            return Ok(LeaseRegistry {
                version: 1,
                updated_at: DateTime::<Utc>::UNIX_EPOCH.to_rfc3339(),
                leases: Vec::new(),
            });
        }
        let mut data = Vec::new();
        File::open(&self.path)?.read_to_end(&mut data)?;
        let registry: LeaseRegistry = serde_json::from_slice(&data)?;
        if registry.version != 1 {
            bail!("unsupported lease registry version");
        }
        Ok(registry)
    }

    fn write_atomic(&self, registry: &LeaseRegistry) -> Result<()> {
        let temporary = self
            .path
            .with_extension(format!("{}.tmp", std::process::id()));
        let mut file = File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, registry)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(temporary, &self.path)?;
        sync_parent(&self.path)?;
        Ok(())
    }
}

fn sync_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::LockScope;
    use chrono::Duration as ChronoDuration;

    fn lease(run_id: &str, agent_id: &str, worktree: &str, branch: &str) -> AgentLease {
        AgentLease {
            agent_id: agent_id.to_owned(),
            run_id: run_id.to_owned(),
            worktree_path: worktree.to_owned(),
            branch: branch.to_owned(),
            owner: "test".to_owned(),
            heartbeat: "2026-08-13T00:00:00Z".to_owned(),
            state: LeaseState::Active,
            lock_scopes: vec![LockScope::Worktree, LockScope::Branch],
            deployment_target: None,
            build_concurrency_group: None,
            scope_keys: Vec::new(),
        }
    }

    fn tmp_store() -> (tempfile::TempDir, LeaseStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = LeaseStore::new(dir.path().join("leases.json"));
        (dir, store)
    }

    #[test]
    fn shared_scope_key_conflicts_even_across_distinct_worktrees() {
        let (_d, store) = tmp_store();
        let mut a = lease("run-a", "agent-a", "wt/a", "agents/a");
        a.scope_keys = vec!["dossier:1/hyp:2".to_owned(), "dossier:1/hyp:3".to_owned()];
        store.acquire(a).unwrap();

        let mut b = lease("run-b", "agent-b", "wt/b", "agents/b");
        b.scope_keys = vec!["dossier:1/hyp:3".to_owned()];
        let err = store.acquire(b).unwrap_err().to_string();
        assert!(
            err.contains("scope `dossier:1/hyp:3` conflict with run-a"),
            "{err}"
        );

        let mut c = lease("run-c", "agent-c", "wt/c", "agents/c");
        c.scope_keys = vec!["dossier:1/hyp:4".to_owned()];
        store.acquire(c).unwrap();
        assert_eq!(store.list().unwrap().len(), 2);
    }

    #[test]
    fn rejects_duplicate_worktree_and_branch() {
        let (_d, store) = tmp_store();
        store
            .acquire(lease("a", "ag-a", "/wt/a", "agents/a"))
            .unwrap();
        let dup_branch = lease("b", "ag-b", "/wt/b", "agents/a");
        assert!(store.acquire(dup_branch).is_err());
        let dup_worktree = lease("b", "ag-b", "/wt/a", "agents/b");
        assert!(store.acquire(dup_worktree).is_err());
    }

    #[test]
    fn heartbeat_updates_and_rejects_released() {
        let (_d, store) = tmp_store();
        store
            .acquire(lease("a", "ag-a", "/wt/a", "agents/a"))
            .unwrap();
        let newer = Utc::now() + ChronoDuration::minutes(1);
        let updated = store.heartbeat("a", newer).unwrap();
        assert_eq!(updated.heartbeat, newer.to_rfc3339());
        store.release("a", Utc::now()).unwrap();
        assert!(store.heartbeat("a", Utc::now()).is_err());
    }

    #[test]
    fn release_unknown_run_is_error() {
        let (_d, store) = tmp_store();
        assert!(store.release("missing", Utc::now()).is_err());
    }

    #[test]
    fn expire_stale_only_flips_active_old_leases() {
        let (_d, store) = tmp_store();
        store
            .acquire(lease("old", "ag-a", "/wt/old", "agents/old"))
            .unwrap();
        store
            .acquire(lease("fresh", "ag-b", "/wt/fresh", "agents/fresh"))
            .unwrap();
        store.heartbeat("fresh", Utc::now()).unwrap();
        let expired = store.expire_stale(Utc::now(), 60_000).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].run_id, "old");
        assert_eq!(
            store
                .list()
                .unwrap()
                .iter()
                .find(|l| l.run_id == "old")
                .unwrap()
                .state,
            LeaseState::Expired
        );
    }

    #[test]
    fn persisted_registry_sorts_by_run_id() {
        let (_d, store) = tmp_store();
        store
            .acquire(lease("z", "ag-z", "/wt/z", "agents/z"))
            .unwrap();
        store
            .acquire(lease("a", "ag-a", "/wt/a", "agents/a"))
            .unwrap();
        let ids: Vec<String> = store
            .list()
            .unwrap()
            .into_iter()
            .map(|l| l.run_id)
            .collect();
        assert_eq!(ids, vec!["a".to_owned(), "z".to_owned()]);
    }
}
