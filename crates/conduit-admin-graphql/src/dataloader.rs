use std::collections::{HashMap, HashSet};
use std::hash::Hash;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BatchKey {
    User { user_id: String },
    Project { project_id: String },
    Channel { channel_id: String },
    Model { model_id: String },
    Request { request_id: String },
    UserProjects { user_id: String },
    ProjectChannels { project_id: String },
    ChannelModels { channel_id: String },
    ModelRequests { model_id: String },
    UserRequests { user_id: String },
}

pub trait BatchLoader {
    type Key: Clone + Eq + Hash;
    type Value: Clone;

    fn load_unique(&self, keys: &[Self::Key]) -> HashMap<Self::Key, Self::Value>;
}

pub fn load_batch<L>(loader: &L, keys: &[L::Key]) -> Vec<Option<L::Value>>
where
    L: BatchLoader,
{
    let unique_keys = deduplicate_keys(keys);
    let loaded = loader.load_unique(&unique_keys);

    keys.iter().map(|key| loaded.get(key).cloned()).collect()
}

fn deduplicate_keys<K>(keys: &[K]) -> Vec<K>
where
    K: Clone + Eq + Hash,
{
    let mut seen = HashSet::with_capacity(keys.len());
    let mut unique_keys = Vec::new();

    for key in keys {
        if seen.insert(key.clone()) {
            unique_keys.push(key.clone());
        }
    }

    unique_keys
}

// =====================================================================
// S09/S13 — Generic DataLoader batching accumulator.
//
// The Go codebase does NOT ship an ent/gqlgen DataLoader (no
// `ent/dataloader` or `graph/dataloader` package exists under
// `conduit/internal/`). The frontend, however, issues dashboard queries that
// fan out across user/project/channel/model/request edges — e.g.
// `dashboard.resolvers.go::DashboardOverview` walks `r.client.Request.Query()`
// and `r.client.UsageLog.Query()` repeatedly per row. The Go privacy/scope
// layer is the only thing standing between a resolver and an N+1 storm.
//
// For the Rust port (S09/S13) we provide a pure batching accumulator that a
// future service-backed resolver can drive: given N keyed loads issued within
// a single dispatch cycle, it (a) dedupes keys so the backing fetch is
// called once per unique key, (b) preserves the caller's request order in
// the returned values, and (c) surfaces missing keys as `None` so a
// resolver can decide whether to skip or error. This mirrors the contract
// documented in TODO_SMALL S09 ("at least users/projects/channels/models/
// request edges implement batching") and S13 ("avoid dashboard N+1").
// =====================================================================

/// Generic DataLoader batcher. `K` is the key type (e.g. user id, project
/// id); `V` is the value type returned by the backing fetch. The batcher is
/// pure: it holds no state between calls and performs no I/O.
///
/// Design note: the Go side has no DataLoader, so this is the canonical
/// Rust-side abstraction for S09/S13. It is intentionally closure-driven
/// (`batch(..., fetch: impl Fn(&[K]) -> Map<K,V>)`) so a service can plug in
/// a real batched repo lookup later without the batcher knowing about repos.
#[derive(Debug, Default, Clone, Copy)]
pub struct DataLoaderBatcher<K, V> {
    _key: std::marker::PhantomData<K>,
    _value: std::marker::PhantomData<V>,
}

impl<K, V> DataLoaderBatcher<K, V>
where
    K: Clone + Eq + Hash,
    V: Clone,
{
    /// Execute a batch of keyed loads against `fetch`, returning one entry
    /// per input key in the **same order** as `loads`.
    ///
    /// Semantics (mirrors the `batch_loader_deduplicates_*` tests above and
    /// the S09/S13 contract):
    /// 1. **Dedupe**: `fetch` is called exactly once with the unique subset
    ///    of `loads`, preserving first-occurrence order. Repeated keys never
    ///    trigger a second fetch.
    /// 2. **Order**: the returned `Vec<Option<V>>` is aligned 1:1 with the
    ///    input `loads` order — `values[i]` is the result for `loads[i]`.
    /// 3. **Missing**: a key the fetch did not return becomes `None`, so the
    ///    caller can distinguish "absent" from "fetched but falsy".
    pub fn batch<F>(loads: &[K], fetch: F) -> Vec<Option<V>>
    where
        F: Fn(&[K]) -> std::collections::HashMap<K, V>,
    {
        let unique_keys = deduplicate_keys(loads);
        let fetched = fetch(&unique_keys);

        loads.iter().map(|key| fetched.get(key).cloned()).collect()
    }

    /// Same as [`batch`](Self::batch) but also returns the keys the fetch did
    /// not surface, so a resolver can log/audit the miss. Mirrors the
    /// `batch_loader_returns_none_for_missing_items` golden intent, plus the
    /// S09 requirement that missing keys be *visible* to the caller.
    pub fn batch_with_missing<F>(loads: &[K], fetch: F) -> (Vec<Option<V>>, Vec<K>)
    where
        F: Fn(&[K]) -> std::collections::HashMap<K, V>,
    {
        let unique_keys = deduplicate_keys(loads);
        let fetched = fetch(&unique_keys);

        let values: Vec<Option<V>> = loads.iter().map(|key| fetched.get(key).cloned()).collect();

        let missing: Vec<K> = unique_keys
            .into_iter()
            .filter(|key| !fetched.contains_key(key))
            .collect();

        (values, missing)
    }
}

/// Free-function form of [`DataLoaderBatcher::batch`], for callers that don't
/// want to name the generic type parameters.
pub fn batch_loads<K, V, F>(loads: &[K], fetch: F) -> Vec<Option<V>>
where
    K: Clone + Eq + Hash,
    V: Clone,
    F: Fn(&[K]) -> std::collections::HashMap<K, V>,
{
    DataLoaderBatcher::<K, V>::batch(loads, fetch)
}

#[cfg(test)]
mod tests {
    use super::{BatchKey, BatchLoader, DataLoaderBatcher, load_batch};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[derive(Default)]
    struct RecordingLoader {
        responses: HashMap<BatchKey, String>,
        calls: RefCell<Vec<Vec<BatchKey>>>,
    }

    impl RecordingLoader {
        fn with_response(mut self, key: BatchKey, value: impl Into<String>) -> Self {
            self.responses.insert(key, value.into());
            self
        }
    }

    impl BatchLoader for RecordingLoader {
        type Key = BatchKey;
        type Value = String;

        fn load_unique(&self, keys: &[Self::Key]) -> HashMap<Self::Key, Self::Value> {
            self.calls.borrow_mut().push(keys.to_vec());

            keys.iter()
                .filter_map(|key| {
                    self.responses
                        .get(key)
                        .map(|value| (key.clone(), value.clone()))
                })
                .collect()
        }
    }

    #[test]
    fn batch_loader_deduplicates_repeated_keys_before_loading() {
        let user_key = BatchKey::User {
            user_id: "user-1".to_owned(),
        };
        let project_edge_key = BatchKey::UserProjects {
            user_id: "user-1".to_owned(),
        };
        let loader = RecordingLoader::default()
            .with_response(user_key.clone(), "user")
            .with_response(project_edge_key.clone(), "projects");

        let values = load_batch(
            &loader,
            &[
                user_key.clone(),
                project_edge_key.clone(),
                user_key.clone(),
                project_edge_key.clone(),
            ],
        );

        assert_eq!(
            loader.calls.borrow().as_slice(),
            &[vec![user_key.clone(), project_edge_key.clone()]]
        );
        assert_eq!(
            values,
            vec![
                Some("user".to_owned()),
                Some("projects".to_owned()),
                Some("user".to_owned()),
                Some("projects".to_owned())
            ]
        );
    }

    #[test]
    fn batch_loader_preserves_request_order() {
        let keys = vec![
            BatchKey::Model {
                model_id: "model-2".to_owned(),
            },
            BatchKey::ProjectChannels {
                project_id: "project-1".to_owned(),
            },
            BatchKey::ChannelModels {
                channel_id: "channel-9".to_owned(),
            },
        ];
        let loader = RecordingLoader::default()
            .with_response(keys[0].clone(), "model")
            .with_response(keys[1].clone(), "channels")
            .with_response(keys[2].clone(), "models");

        let values = load_batch(&loader, &keys);

        assert_eq!(
            values,
            vec![
                Some("model".to_owned()),
                Some("channels".to_owned()),
                Some("models".to_owned())
            ]
        );
    }

    #[test]
    fn batch_loader_returns_none_for_missing_items() {
        let request_key = BatchKey::Request {
            request_id: "request-3".to_owned(),
        };
        let missing_edge_key = BatchKey::ModelRequests {
            model_id: "model-missing".to_owned(),
        };
        let user_requests_key = BatchKey::UserRequests {
            user_id: "user-1".to_owned(),
        };
        let loader = RecordingLoader::default()
            .with_response(request_key.clone(), "request")
            .with_response(user_requests_key.clone(), "requests");

        let values = load_batch(
            &loader,
            &[
                request_key.clone(),
                missing_edge_key.clone(),
                user_requests_key.clone(),
            ],
        );

        assert_eq!(
            values,
            vec![
                Some("request".to_owned()),
                None,
                Some("requests".to_owned())
            ]
        );
    }

    // -----------------------------------------------------------------
    // S09/S13 — generic DataLoaderBatcher closure API. These tests mirror
    // the three golden intents above (dedupe / order / missing) but drive
    // the new closure-driven `DataLoaderBatcher::batch` so the S09/S13
    // contract holds independently of the `BatchLoader` trait.
    // -----------------------------------------------------------------

    #[allow(clippy::type_complexity)] // test helper: closure + Rc<RefCell<…>> is inherent
    fn recorder(
        responses: HashMap<String, String>,
    ) -> (
        impl Fn(&[String]) -> HashMap<String, String>,
        Rc<RefCell<Vec<Vec<String>>>>,
    ) {
        let calls = Rc::new(RefCell::new(Vec::<Vec<String>>::new()));
        let calls_clone = calls.clone();
        let fetch = move |keys: &[String]| -> HashMap<String, String> {
            calls_clone.borrow_mut().push(keys.to_vec());
            keys.iter()
                .filter_map(|k| responses.get(k).map(|v| (k.clone(), v.clone())))
                .collect()
        };
        (fetch, calls)
    }

    #[test]
    fn dataloader_batcher_dedupes_keys_before_calling_fetch() {
        // S09: N+1 avoidance — the fetch closure is invoked exactly once
        // with the deduped key set, no matter how many times a key repeats
        // in the input cycle.
        let responses: HashMap<String, String> = [("user-1".to_owned(), "Alice".to_owned())]
            .into_iter()
            .collect();
        let (fetch, calls) = recorder(responses);

        let loads = vec![
            "user-1".to_owned(),
            "user-1".to_owned(),
            "user-1".to_owned(),
        ];
        let values = DataLoaderBatcher::<String, String>::batch(&loads, fetch);

        assert_eq!(
            calls.borrow().as_slice(),
            &[vec!["user-1".to_owned()]],
            "fetch should be called once with the deduped key set"
        );
        assert_eq!(
            values,
            vec![
                Some("Alice".to_owned()),
                Some("Alice".to_owned()),
                Some("Alice".to_owned())
            ]
        );
    }

    #[test]
    fn dataloader_batcher_preserves_input_order_in_output() {
        // S13: dashboard N+1 — even when the fetch returns a HashMap
        // (unordered), the batcher must hand back values aligned to the
        // caller's request order so resolvers can map edge -> node.
        let responses: HashMap<String, String> = [
            ("model-2".to_owned(), "M2".to_owned()),
            ("project-1".to_owned(), "P1".to_owned()),
            ("channel-9".to_owned(), "C9".to_owned()),
        ]
        .into_iter()
        .collect();
        let (fetch, _calls) = recorder(responses);

        let loads = vec![
            "model-2".to_owned(),
            "project-1".to_owned(),
            "channel-9".to_owned(),
        ];
        let values = DataLoaderBatcher::<String, String>::batch(&loads, fetch);

        assert_eq!(
            values,
            vec![
                Some("M2".to_owned()),
                Some("P1".to_owned()),
                Some("C9".to_owned())
            ]
        );
    }

    #[test]
    fn dataloader_batcher_surfaces_missing_keys_as_none() {
        // S09: missing keys must be visible as `None`, not silently dropped,
        // so the resolver can decide to skip or error per Go's behavior.
        let responses: HashMap<String, String> = [
            ("user-1".to_owned(), "Alice".to_owned()),
            ("user-3".to_owned(), "Carol".to_owned()),
        ]
        .into_iter()
        .collect();
        let (fetch, _calls) = recorder(responses);

        let loads = vec![
            "user-1".to_owned(),
            "user-2".to_owned(), // missing
            "user-3".to_owned(),
        ];
        let values = DataLoaderBatcher::<String, String>::batch(&loads, fetch);

        assert_eq!(
            values,
            vec![Some("Alice".to_owned()), None, Some("Carol".to_owned())]
        );
    }

    #[test]
    fn dataloader_batcher_with_missing_reports_missing_keys() {
        // S09/S13 audit surface: `batch_with_missing` returns the list of
        // keys the fetch did NOT surface, deduped, so a resolver can log or
        // 404 precisely.
        let responses: HashMap<String, String> = [("present".to_owned(), "v".to_owned())]
            .into_iter()
            .collect();
        let (fetch, _calls) = recorder(responses);

        let loads = vec![
            "present".to_owned(),
            "absent-a".to_owned(),
            "absent-b".to_owned(),
            "absent-a".to_owned(), // dup missing — must appear once in report
        ];
        let (values, missing) =
            DataLoaderBatcher::<String, String>::batch_with_missing(&loads, fetch);

        assert_eq!(values, vec![Some("v".to_owned()), None, None, None]);
        // Missing list is deduped and order-stable (first-occurrence order).
        assert_eq!(missing.len(), 2);
        assert!(missing.contains(&"absent-a".to_owned()));
        assert!(missing.contains(&"absent-b".to_owned()));
    }

    #[test]
    fn dataloader_batcher_handles_empty_load_set() {
        // Edge case: empty input must produce empty output and still call
        // fetch once with an empty slice (no N+1 panic on zero keys).
        let (fetch, calls) = recorder(HashMap::new());

        let values = DataLoaderBatcher::<String, String>::batch(&[], fetch);

        assert!(values.is_empty());
        assert_eq!(calls.borrow().as_slice(), &[Vec::<String>::new()]);
    }

    #[test]
    fn dataloader_batcher_preserves_order_with_mixed_hits_and_misses() {
        // S13 contract under stress: dedupe + order + missing must all hold
        // simultaneously, mirroring dashboard edges where some users have
        // projects and others don't.
        let responses: HashMap<String, String> = [
            ("u-1".to_owned(), "u1".to_owned()),
            ("u-3".to_owned(), "u3".to_owned()),
            ("u-5".to_owned(), "u5".to_owned()),
        ]
        .into_iter()
        .collect();
        let (fetch, _calls) = recorder(responses);

        let loads = vec![
            "u-1".to_owned(),
            "u-2".to_owned(), // miss
            "u-3".to_owned(),
            "u-1".to_owned(), // dup hit
            "u-4".to_owned(), // miss
            "u-5".to_owned(),
        ];
        let values = DataLoaderBatcher::<String, String>::batch(&loads, fetch);

        assert_eq!(
            values,
            vec![
                Some("u1".to_owned()),
                None,
                Some("u3".to_owned()),
                Some("u1".to_owned()),
                None,
                Some("u5".to_owned())
            ]
        );
    }
}
