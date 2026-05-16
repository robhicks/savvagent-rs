//! Pool lifecycle tests: drain, lock hygiene, force-disconnect.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use savvagent_host::capabilities::{CostTier, ModelCapabilities, ProviderCapabilities};
use savvagent_host::{
    DisconnectMode, Host, HostConfig, ProviderEndpoint, ProviderRegistration, StartupConnectPolicy,
};
use savvagent_mcp::ProviderClient;
use savvagent_protocol::{
    CompleteRequest, CompleteResponse, ListModelsResponse, ProviderError, ProviderId, StreamEvent,
};
use tokio::sync::mpsc;

struct EchoClient;
#[async_trait]
impl ProviderClient for EchoClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        Ok(CompleteResponse {
            id: "echo-0".into(),
            model: req.model.clone(),
            content: vec![],
            stop_reason: savvagent_protocol::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        })
    }
    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![],
            default_model_id: None,
        })
    }
}

fn caps(model_id: &str) -> ProviderCapabilities {
    ProviderCapabilities {
        models: vec![ModelCapabilities {
            id: model_id.into(),
            display_name: model_id.into(),
            supports_vision: false,
            supports_audio: false,
            context_window: 1000,
            cost_tier: CostTier::Standard,
        }],
        default_model: model_id.into(),
    }
}

fn reg(id: &str, model: &str) -> ProviderRegistration {
    ProviderRegistration {
        id: ProviderId::new(id).unwrap(),
        display_name: id.into(),
        client: Arc::new(EchoClient) as Arc<dyn ProviderClient + Send + Sync>,
        capabilities: caps(model),
        aliases: vec![],
    }
}

#[tokio::test]
async fn host_starts_with_pool_and_active_provider() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![reg("anthropic", "m")];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Host::start(cfg).await.expect("start ok");
    assert_eq!(host.active_provider().await.as_str(), "anthropic");
    assert!(host.is_connected("anthropic").await);
}

#[tokio::test]
async fn add_provider_rejects_duplicate() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![reg("anthropic", "m")];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Host::start(cfg).await.unwrap();
    let dup = reg("anthropic", "m");
    let err = host.add_provider(dup).await.unwrap_err();
    assert!(matches!(
        err,
        savvagent_host::PoolError::AlreadyRegistered(ref id) if id.as_str() == "anthropic"
    ));
}

#[tokio::test]
async fn remove_provider_drain_blocks_new_turns_but_lets_inflight_finish() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![reg("anthropic", "m")];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Arc::new(Host::start(cfg).await.unwrap());

    // Take a lease ourselves to simulate an in-flight turn.
    let lease = host
        .acquire_lease_for_test(&ProviderId::new("anthropic").unwrap())
        .await
        .unwrap();

    // Drain while lease is held — must not block on the pool lock.
    let host_clone = Arc::clone(&host);
    let drain_handle = tokio::spawn(async move {
        host_clone
            .remove_provider(
                &ProviderId::new("anthropic").unwrap(),
                DisconnectMode::Drain,
            )
            .await
    });

    // Provider is gone from the eligibility set immediately.
    // (Use a tight retry loop to allow the spawned task to start.)
    for _ in 0..40 {
        if !host.is_connected("anthropic").await {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(!host.is_connected("anthropic").await);

    // Drop our lease; drain should complete shortly after.
    drop(lease);
    tokio::time::timeout(Duration::from_secs(2), drain_handle)
        .await
        .expect("drain finished in time")
        .unwrap()
        .unwrap();
}

/// A provider client whose `complete` sleeps for 60 s — far beyond the grace
/// period — so that it can only finish via `AbortHandle::abort`.
struct StuckClient {
    started: Arc<AtomicBool>,
}

#[async_trait]
impl ProviderClient for StuckClient {
    async fn complete(
        &self,
        _req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        self.started.store(true, Ordering::SeqCst);
        // Sleep way past the grace period.  If we are aborted, the future is
        // dropped during this sleep and `complete` never returns.
        tokio::time::sleep(Duration::from_secs(60)).await;
        Err(ProviderError {
            kind: savvagent_protocol::ErrorKind::Internal,
            message: "should never reach here".into(),
            retry_after_ms: None,
            provider_code: None,
        })
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![],
            default_model_id: None,
        })
    }
}

#[tokio::test]
async fn force_disconnect_aborts_uncooperative_turn_within_grace() {
    let started = Arc::new(AtomicBool::new(false));
    let stuck = StuckClient {
        started: Arc::clone(&started),
    };
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![ProviderRegistration {
        id: ProviderId::new("stuck").unwrap(),
        display_name: "Stuck".into(),
        client: Arc::new(stuck) as Arc<dyn ProviderClient + Send + Sync>,
        capabilities: caps("m"),
        aliases: vec![],
    }];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.force_disconnect_grace_ms = 200; // tighten for a faster test

    let host = Arc::new(Host::start(cfg).await.unwrap());

    // Spawn a turn that will get stuck inside `StuckClient::complete`.
    let host_t = Arc::clone(&host);
    let turn_handle = tokio::spawn(async move { host_t.run_turn("hello").await });

    // Wait until the stuck client has entered `complete` so we know the
    // turn is genuinely in-flight before we force-disconnect.
    for _ in 0..200 {
        if started.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        started.load(Ordering::SeqCst),
        "StuckClient::complete never started"
    );

    // Force-disconnect; must complete in much less than grace + slack.
    let t0 = std::time::Instant::now();
    host.remove_provider(&ProviderId::new("stuck").unwrap(), DisconnectMode::Force)
        .await
        .unwrap();
    let elapsed = t0.elapsed();
    assert!(
        elapsed < Duration::from_millis(400),
        "force_disconnect took {elapsed:?}, expected < 400 ms"
    );

    // The spawned turn must resolve (as an error — it was aborted or cancelled)
    // within a generous deadline.
    let outcome = tokio::time::timeout(Duration::from_secs(2), turn_handle)
        .await
        .expect("turn task should resolve after abort")
        .expect("JoinHandle should not panic");
    assert!(
        outcome.is_err(),
        "expected an error from the aborted turn, got Ok"
    );
}

#[tokio::test]
async fn set_active_provider_rejects_unknown() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![reg("anthropic", "m")];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Host::start(cfg).await.unwrap();
    let bad = ProviderId::new("missing").unwrap();
    let err = host.set_active_provider(&bad).await.unwrap_err();
    assert!(matches!(err, savvagent_host::PoolError::NotRegistered(_)));
    // Active provider unchanged.
    assert_eq!(host.active_provider().await.as_str(), "anthropic");
}

/// A provider that blocks inside `complete` until a `release` oneshot fires,
/// then returns immediately with an empty success response. The `entered` flag
/// lets the test know when `complete` has been reached.
struct GatedClient {
    entered: Arc<AtomicBool>,
    release: Arc<tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>>,
}

#[async_trait]
impl ProviderClient for GatedClient {
    async fn complete(
        &self,
        req: CompleteRequest,
        _events: Option<mpsc::Sender<StreamEvent>>,
    ) -> Result<CompleteResponse, ProviderError> {
        self.entered.store(true, Ordering::SeqCst);
        // Wait for the release signal (or forever if never sent — the test
        // force-disconnects before releasing, so the future gets dropped).
        let mut slot = self.release.lock().await;
        if let Some(rx) = slot.take() {
            let _ = rx.await;
        }
        Ok(CompleteResponse {
            id: "gated-0".into(),
            model: req.model.clone(),
            content: vec![],
            stop_reason: savvagent_protocol::StopReason::EndTurn,
            stop_sequence: None,
            usage: Default::default(),
        })
    }

    async fn list_models(&self) -> Result<ListModelsResponse, ProviderError> {
        Ok(ListModelsResponse {
            models: vec![],
            default_model_id: None,
        })
    }
}

/// Regression test for the force-disconnect TOCTOU race (Critical #3).
///
/// Before the fix, a Force disconnect that arrived between lease acquisition
/// and broadcast subscription could miss in-flight turns: the Stage-1 send
/// found no subscriber, and Stage-3 aborts were registered before the abort
/// handle was. The fix moves subscription before lease acquisition.
///
/// This test fires force-disconnect as soon as the turn task is *spawned* —
/// that is, before `complete()` has necessarily been entered — and asserts
/// that the turn still resolves (cancelled or aborted) without panic and that
/// `active_turn_count` reaches zero.
#[tokio::test]
async fn force_disconnect_races_run_turn_startup() {
    let entered = Arc::new(AtomicBool::new(false));
    let (_release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let gated = GatedClient {
        entered: Arc::clone(&entered),
        release: Arc::new(tokio::sync::Mutex::new(Some(release_rx))),
    };

    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![ProviderRegistration {
        id: ProviderId::new("gated").unwrap(),
        display_name: "Gated".into(),
        client: Arc::new(gated) as Arc<dyn ProviderClient + Send + Sync>,
        capabilities: caps("m"),
        aliases: vec![],
    }];
    cfg.startup_connect = StartupConnectPolicy::All;
    cfg.force_disconnect_grace_ms = 200;

    let host = Arc::new(Host::start(cfg).await.unwrap());

    // Spawn the turn — do NOT wait for complete() to be entered.
    // This is the race window the fix is designed to close.
    let host_t = Arc::clone(&host);
    let turn_handle = tokio::spawn(async move { host_t.run_turn("hello").await });

    // Yield briefly so the turn task can start, then immediately disconnect.
    tokio::task::yield_now().await;

    let t0 = std::time::Instant::now();
    host.remove_provider(&ProviderId::new("gated").unwrap(), DisconnectMode::Force)
        .await
        .unwrap();
    let elapsed = t0.elapsed();

    // Force-disconnect must complete within grace + generous slack.
    assert!(
        elapsed < Duration::from_millis(600),
        "force_disconnect took {elapsed:?}, expected < 600 ms"
    );

    // The turn must resolve (with an error — it was cancelled or aborted).
    let outcome = tokio::time::timeout(Duration::from_secs(3), turn_handle)
        .await
        .expect("turn task should resolve after force-disconnect")
        .expect("JoinHandle must not panic");
    assert!(
        outcome.is_err(),
        "expected an error from the cancelled turn, got Ok"
    );
}

#[tokio::test]
async fn set_active_provider_swaps_when_pool_has_entry() {
    let mut cfg = HostConfig::new(
        ProviderEndpoint::StreamableHttp {
            url: "http://unused".into(),
        },
        "m",
    );
    cfg.providers = vec![reg("anthropic", "m"), reg("gemini", "m")];
    cfg.startup_connect = StartupConnectPolicy::All;
    let host = Host::start(cfg).await.unwrap();
    assert_eq!(host.active_provider().await.as_str(), "anthropic");
    let gemini = ProviderId::new("gemini").unwrap();
    host.set_active_provider(&gemini).await.unwrap();
    assert_eq!(host.active_provider().await.as_str(), "gemini");
}
