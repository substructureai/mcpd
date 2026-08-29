use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

const SWEEP_THRESHOLD: usize = 64;

#[derive(Default)]
pub struct Locks {
    keys: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
}

pub type Held = Vec<OwnedMutexGuard<()>>;

impl Locks {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn acquire(&self, keys: &[String]) -> Held {
        let mut wanted: Vec<&str> = keys.iter().map(String::as_str).collect();
        wanted.sort_unstable();
        wanted.dedup();

        let mut held = Vec::with_capacity(wanted.len());
        for key in wanted {
            held.push(self.mutex(key).lock_owned().await);
        }
        held
    }

    fn mutex(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut keys = self
            .keys
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        if let Some(live) = keys.get(key).and_then(Weak::upgrade) {
            return live;
        }

        if keys.len() >= SWEEP_THRESHOLD {
            keys.retain(|_, entry| entry.strong_count() > 0);
        }

        let created = Arc::new(AsyncMutex::new(()));
        keys.insert(key.to_string(), Arc::downgrade(&created));
        created
    }

    #[cfg(test)]
    fn tracked(&self) -> usize {
        self.keys.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn keys(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[derive(Default)]
    struct Overlap {
        inside: AtomicUsize,
        peak: AtomicUsize,
    }

    impl Overlap {
        async fn enter(&self) {
            let now = self.inside.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(20)).await;
            self.inside.fetch_sub(1, Ordering::SeqCst);
        }

        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }
    }

    #[tokio::test]
    async fn one_key_admits_one_caller_at_a_time() {
        let locks = Locks::new();
        let overlap = Overlap::default();

        let a = async {
            let _held = locks.acquire(&keys(&["f"])).await;
            overlap.enter().await;
        };
        let b = async {
            let _held = locks.acquire(&keys(&["f"])).await;
            overlap.enter().await;
        };
        tokio::join!(a, b);

        assert_eq!(overlap.peak(), 1);
    }

    #[tokio::test]
    async fn different_keys_do_not_block_each_other() {
        let locks = Locks::new();
        let overlap = Overlap::default();

        let a = async {
            let _held = locks.acquire(&keys(&["one"])).await;
            overlap.enter().await;
        };
        let b = async {
            let _held = locks.acquire(&keys(&["two"])).await;
            overlap.enter().await;
        };
        tokio::join!(a, b);

        assert_eq!(overlap.peak(), 2);
    }

    #[tokio::test]
    async fn a_key_named_twice_in_one_call_is_taken_once() {
        let locks = Locks::new();
        let held = tokio::time::timeout(
            Duration::from_secs(5),
            locks.acquire(&keys(&["f", "f", "f"])),
        )
        .await
        .expect("deadlocked on its own key");

        assert_eq!(held.len(), 1);
    }

    #[tokio::test]
    async fn keys_are_taken_in_a_consistent_order() {
        let locks = Locks::new();

        let a = async {
            let _held = locks.acquire(&keys(&["a", "b"])).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        let b = async {
            let _held = locks.acquire(&keys(&["b", "a"])).await;
            tokio::time::sleep(Duration::from_millis(20)).await;
        };

        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(a, b) })
            .await
            .expect("deadlocked acquiring two keys");
    }

    #[tokio::test]
    async fn a_key_nobody_holds_is_eventually_swept() {
        let locks = Locks::new();

        for i in 0..=SWEEP_THRESHOLD {
            drop(locks.acquire(&keys(&[&format!("k{i}")])).await);
        }

        assert!(
            locks.tracked() < SWEEP_THRESHOLD,
            "dead keys accumulated: {}",
            locks.tracked()
        );
    }

    #[tokio::test]
    async fn a_call_naming_no_keys_holds_nothing() {
        let locks = Locks::new();
        assert!(locks.acquire(&[]).await.is_empty());
        assert_eq!(locks.tracked(), 0);
    }
}
