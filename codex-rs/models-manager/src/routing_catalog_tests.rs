use super::*;
use pretty_assertions::assert_eq;

#[derive(Debug)]
struct RoutingEndpoint {
    inner: Arc<TestModelsEndpoint>,
    identity: Mutex<Option<String>>,
}

impl ModelsEndpointClient for RoutingEndpoint {
    fn identity(&self) -> Option<String> {
        self.identity.lock().unwrap().clone()
    }

    fn has_command_auth(&self) -> bool {
        false
    }

    fn uses_codex_backend(&self) -> ModelsEndpointFuture<'_, bool> {
        Box::pin(std::future::ready(true))
    }

    fn list_models<'a>(
        &'a self,
        _client_version: &'a str,
        _factory: HttpClientFactory,
    ) -> ModelsEndpointFuture<'a, CoreResult<ModelsEndpointResponse>> {
        Box::pin(async {
            let mut response = self.inner.list_models().await?;
            response.identity = self.identity().unwrap();
            Ok(response)
        })
    }
}

#[tokio::test]
async fn automatic_routing_requires_discovery_and_discards_a_previous_owner() {
    let home = tempdir().unwrap();
    let model = remote_model("verified-model", "Verified", /*priority*/ 0);
    let endpoint = Arc::new(RoutingEndpoint {
        inner: TestModelsEndpoint::new(vec![vec![model.clone()]]),
        identity: Mutex::new(Some("owner-a".into())),
    });
    let manager = openai_manager_for_tests(home.path().into(), endpoint.clone());
    assert_eq!(
        manager
            .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
            .await,
        None
    );
    assert_eq!(endpoint.inner.fetch_count(), 0);
    manager
        .raw_model_catalog(RefreshStrategy::Online, DEFAULT_HTTP_CLIENT_FACTORY)
        .await;
    assert_eq!(
        manager
            .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
            .await,
        Some(ModelsResponse {
            models: vec![model.clone()]
        })
    );
    let restored = openai_manager_for_tests(home.path().into(), endpoint.clone());
    assert_eq!(
        restored
            .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
            .await,
        Some(ModelsResponse {
            models: vec![model]
        })
    );
    *endpoint.identity.lock().unwrap() = Some("owner-b".into());
    assert_eq!(
        manager
            .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
            .await,
        None
    );
    assert_eq!(
        restored
            .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
            .await,
        None
    );
    assert_eq!(endpoint.inner.fetch_count(), 1);
}

#[tokio::test]
async fn automatic_routing_excludes_bundled_models_merged_into_hidden_remote_catalog() {
    let endpoint = TestModelsEndpoint::new(vec![vec![remote_model_with_visibility(
        "hidden", "Hidden", /*priority*/ 0, "hide",
    )]]);
    let manager = OpenAiModelsManager::new_without_cache(endpoint, /*auth_manager*/ None);
    let ordinary = manager
        .raw_model_catalog(RefreshStrategy::Online, DEFAULT_HTTP_CLIENT_FACTORY)
        .await;
    assert!(ordinary.models.len() > 1);
    let verified = manager
        .verified_model_catalog(DEFAULT_HTTP_CLIENT_FACTORY)
        .await
        .unwrap();
    assert_eq!(
        verified.models,
        vec![remote_model_with_visibility(
            "hidden", "Hidden", /*priority*/ 0, "hide"
        )]
    );
}
