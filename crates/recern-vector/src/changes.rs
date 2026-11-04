//! Volatile dirty-node tracking. No persistence identifiers are taken from this epoch.
use std::collections::BTreeSet;
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

pub(crate) struct Changes {
    pub epoch: u64,
    pub tracking: AtomicBool,
    pub nodes: Mutex<BTreeSet<u32>>,
}
impl Changes {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            epoch: NEXT.fetch_add(1, Ordering::Relaxed),
            tracking: AtomicBool::new(false),
            nodes: Mutex::new(BTreeSet::new()),
        }
    }
    pub fn mark(&mut self, node: u32) {
        if *self.tracking.get_mut() {
            self.nodes.get_mut().unwrap().insert(node);
        }
    }
    pub fn saved(&self) {
        self.nodes.lock().unwrap().clear();
        self.tracking.store(true, Ordering::Relaxed);
    }
    pub fn pending(&self) -> BTreeSet<u32> {
        self.nodes.lock().unwrap().clone()
    }
}
