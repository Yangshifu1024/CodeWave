//! Host-issued review tickets bind source, exact configuration and authority revision.
use super::*;
use crate::mcp::trust::{authority_error, authority_key};
use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewAction {
    Once,
    Remember,
    Revoke,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reviews_expire_are_single_use_and_recover_capacity_after_close() {
        let manager = McpManager::default();
        let key = PoolKey::global("fixture");
        let cfg = super::super::tests::node_cfg();
        let version = super::super::super::launch_version(&cfg);
        let mut tickets = Vec::new();
        for _ in 0..64 {
            tickets.push(
                manager
                    .prepare_launch_review(&key, &cfg, &version, ReviewAction::Once)
                    .await
                    .unwrap(),
            );
        }
        assert!(
            manager
                .prepare_launch_review(&key, &cfg, &version, ReviewAction::Once)
                .await
                .is_err()
        );
        manager.discard_launch_review(&tickets[0].id);
        assert!(manager.consume_launch_review(&tickets[0].id).is_err());
        let extra = manager
            .prepare_launch_review(&key, &cfg, &version, ReviewAction::Once)
            .await
            .unwrap();
        assert!(manager.consume_launch_review(&extra.id).is_ok());
        assert!(manager.consume_launch_review(&extra.id).is_err());
        // A consumed ticket waiting for final admission is invalidated too.
        assert!(manager.validate_launch_review(&extra.id).is_ok());
        manager.invalidate_launch_reviews(&key);
        assert!(manager.validate_launch_review(&extra.id).is_err());
        manager.discard_launch_review(&tickets[2].id);
        let queued = manager
            .prepare_launch_review(&key, &cfg, &version, ReviewAction::Once)
            .await
            .unwrap();
        manager.consume_launch_review(&queued.id).unwrap();
        let inner = manager.inner.lock().await;
        let approval = manager.approve_launch_review(&queued.id, &key, &cfg, None);
        tokio::pin!(approval);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), &mut approval)
                .await
                .is_err()
        );
        manager.invalidate_launch_reviews(&key);
        drop(inner);
        assert!(approval.await.is_err());
        assert!(!manager.launch_status(&key, &cfg).trusted);
        manager
            .reviews
            .lock()
            .unwrap()
            .get_mut(&tickets[1].id)
            .unwrap()
            .issued = Instant::now() - Duration::from_secs(301);
        assert!(manager.consume_launch_review(&tickets[1].id).is_err());
        assert!(
            manager
                .prepare_launch_review(&key, &cfg, &version, ReviewAction::Once)
                .await
                .is_ok()
        );
    }
}

#[derive(Serialize)]
pub struct LaunchReview {
    pub id: String,
    pub version: String,
    pub source: String,
    pub name: String,
    pub action: ReviewAction,
    pub config: McpServerConfig,
}

#[derive(Serialize)]
pub struct LaunchReviewReceipt {
    pub operation_id: String,
    pub confirmed: bool,
    pub resolved: bool,
    pub status: Option<LaunchStatus>,
    pub error: Option<String>,
}

#[derive(Clone)]
pub(crate) struct PendingReview {
    pub key: PoolKey,
    pub cfg: McpServerConfig,
    pub action: ReviewAction,
    pub revision: Option<u64>,
    issued: Instant,
    consumed: bool,
    cancelled: bool,
    pub applied: bool,
}

impl McpManager {
    /// Recheck the consumed ticket under the same pool lock as its grant.
    /// Background retirement also takes this lock before cancelling tickets.
    pub(in crate::mcp) async fn approve_launch_review(
        &self,
        id: &str,
        key: &PoolKey,
        cfg: &McpServerConfig,
        revision: Option<u64>,
    ) -> Result<(), McpError> {
        let mut inner = self.inner.lock().await;
        self.validate_launch_review(id)?;
        let permit = if let Some(revision) = revision {
            self.launch.grant_persistent(
                key,
                cfg,
                super::super::trust::AuthorityProof {
                    store: self.authority.as_ref().ok_or_else(untrusted)?.clone(),
                    revision,
                },
            )?
        } else {
            self.launch
                .grant(key, cfg, &super::super::launch_version(cfg))?;
            self.launch.connection(key, cfg)?
        };
        if let Some(entry) = inner.entries.get_mut(key)
            && entry.cfg == *cfg
        {
            entry.launch = Some(permit);
        }
        self.finish_launch_review(id);
        Ok(())
    }

    pub async fn prepare_launch_review(
        &self,
        key: &PoolKey,
        cfg: &McpServerConfig,
        expected: &str,
        action: ReviewAction,
    ) -> Result<LaunchReview, McpError> {
        if super::super::launch_version(cfg) != expected
            || (action != ReviewAction::Revoke && !cfg.enabled)
        {
            return Err(untrusted());
        }
        let revision = if action == ReviewAction::Once {
            None
        } else {
            let store = self
                .authority
                .as_ref()
                .ok_or_else(|| McpError::config("Persistent trust authority is unavailable"))?;
            store
                .read_guard(&authority_key(key), &CancellationToken::new())
                .await
                .map_err(authority_error)?
                .record
                .as_ref()
                .map(|record| record.revision)
        };
        let mut reviews = self.reviews.lock().unwrap();
        reviews.retain(|_, review| review.issued.elapsed() < Duration::from_secs(300));
        if reviews.len() >= 64 {
            return Err(McpError::config(
                "Too many pending launch reviews; close or finish an existing review",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        reviews.insert(
            id.clone(),
            PendingReview {
                key: key.clone(),
                cfg: cfg.clone(),
                action,
                revision,
                issued: Instant::now(),
                consumed: false,
                cancelled: false,
                applied: false,
            },
        );
        Ok(LaunchReview {
            id,
            version: expected.into(),
            source: key.source.display().to_string(),
            name: key.name.clone(),
            action,
            config: cfg.clone(),
        })
    }

    pub(crate) fn consume_launch_review(&self, id: &str) -> Result<PendingReview, McpError> {
        let mut reviews = self.reviews.lock().unwrap();
        let review = reviews.get_mut(id).ok_or_else(untrusted)?;
        if review.consumed || review.cancelled {
            return Err(untrusted());
        }
        review.consumed = true;
        if review.issued.elapsed() >= Duration::from_secs(300) {
            return Err(untrusted());
        }
        Ok(review.clone())
    }

    pub(crate) fn launch_operation(
        &self,
        id: &str,
        key: &PoolKey,
        cfg: &McpServerConfig,
    ) -> Result<PendingReview, McpError> {
        self.reviews
            .lock()
            .unwrap()
            .get(id)
            .filter(|review| {
                review.consumed
                    && review.key == *key
                    && review.cfg == *cfg
                    && review.issued.elapsed() < Duration::from_secs(300)
            })
            .cloned()
            .ok_or_else(untrusted)
    }

    pub(crate) fn finish_launch_review(&self, id: &str) {
        if let Some(review) = self.reviews.lock().unwrap().get_mut(id) {
            review.applied = true;
        }
    }

    pub(crate) fn validate_launch_review(&self, id: &str) -> Result<(), McpError> {
        self.reviews
            .lock()
            .unwrap()
            .get(id)
            .filter(|review| {
                review.consumed
                    && !review.cancelled
                    && review.issued.elapsed() < Duration::from_secs(300)
            })
            .map(|_| ())
            .ok_or_else(untrusted)
    }

    pub(super) fn invalidate_launch_reviews(&self, key: &PoolKey) {
        for review in self.reviews.lock().unwrap().values_mut().filter(|review| {
            review.key == *key && !review.applied && review.action != ReviewAction::Revoke
        }) {
            review.cancelled = true;
        }
    }

    /// Call under the pool lock, including authority failures before Ready.
    pub(super) fn revoke_launch(&self, key: &PoolKey) {
        self.launch.revoke(key);
        self.invalidate_launch_reviews(key);
    }

    pub(super) fn retire_launch(&self, key: &PoolKey, id: &str, revision: Option<u64>) {
        // A stale handshake/authority scan must preserve a newer user approval.
        if self.launch.revoke_if_current(key, id, revision) {
            self.invalidate_launch_reviews(key);
        }
    }

    pub(super) fn invalidate_review_source(
        &self,
        source: &std::path::Path,
        servers: &[(String, McpServerConfig)],
    ) {
        for review in self.reviews.lock().unwrap().values_mut().filter(|review| {
            review.key.source == source
                && !review.applied
                && !servers.iter().any(|(name, cfg)| {
                    name == &review.key.name && cfg == &review.cfg && cfg.enabled
                })
        }) {
            review.cancelled = true;
        }
    }

    pub fn discard_launch_review(&self, id: &str) {
        let mut reviews = self.reviews.lock().unwrap();
        if reviews
            .get(id)
            .is_some_and(|review| !review.consumed || review.applied)
        {
            reviews.remove(id);
        }
    }
}
