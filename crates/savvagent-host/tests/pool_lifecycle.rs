//! Pool lifecycle tests: drain, lock hygiene. Force-mode tests live
//! in Task 5.

use std::sync::Arc;
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
