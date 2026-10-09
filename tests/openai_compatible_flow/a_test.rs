//! Model, embedding and conversation-freezing flows.

use super::*;

#[tokio::test]
pub(crate) async fn exercises_models_embeddings_and_sse_against_a_mock_openai_server() {
    let (base_url, requests, server) = mock_server(3);
    let provider = OpenAiHttpProvider::new(ProviderConfig {
        base_url,
        api_key: Some("test-key".to_string()),
        remote_content_confirmed: false,
        allow_insecure_remote_http: false,
        request_timeout_secs: 10,
    })
    .unwrap();

    let models = provider.models().await.unwrap();
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["chat-model", "embed-model"]
    );

    let embeddings = provider
        .embeddings(EmbeddingRequest {
            model: "embed-model".to_string(),
            input: vec!["first".to_string(), "second".to_string()],
            dimensions: None,
        })
        .await
        .unwrap();
    assert_eq!(embeddings.vectors, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);

    let mut stream = provider
        .chat_stream(ChatRequest {
            model: "chat-model".to_string(),
            messages: vec![ChatMessage::text(ChatRole::User, "question")],
            tools: Vec::new(),
            temperature: None,
            top_p: None,
            max_tokens: Some(32),
            presence_penalty: None,
            frequency_penalty: None,
            reasoning_effort: None,
        })
        .await
        .unwrap();
    let mut text = String::new();
    let mut done = false;
    while let Some(event) = stream.next().await {
        let event = event.unwrap();
        text.push_str(event.content_delta.as_deref().unwrap_or_default());
        done |= event.done;
    }
    assert_eq!(text, "mock answer");
    assert!(done);

    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key")
    }));
    assert!(requests[1].contains("\"input\":[\"first\",\"second\"]"));
    assert!(requests[2].contains("\"stream\":true"));
    let chat = chat_request_body(&requests[2]);
    for field in [
        "temperature",
        "top_p",
        "presence_penalty",
        "frequency_penalty",
    ] {
        assert!(chat.get(field).is_none(), "unset {field} must be omitted");
    }
}

#[test]
pub(crate) fn conversation_freezes_persisted_chat_parameters_before_saved_changes() {
    let (base_url, requests, server) = scripted_chat_server(vec![
        answer_sse("使用已保存的对话参数。"),
        answer_sse("使用下一次保存的对话参数。"),
    ]);
    let temp = tempfile::tempdir().unwrap();
    let credentials = Arc::new(MemoryCredentialStore::default());
    let settings = ProviderSettings {
        base_url,
        chat_model: "configured-chat-model".to_string(),
        chat_generation: custom_chat_generation(),
        request_timeout_secs: 10,
        ..ProviderSettings::default()
    };
    {
        let services =
            AppServices::open_with_credentials(temp.path(), credentials.clone()).unwrap();
        services
            .runtime()
            .block_on(services.configure_provider(settings.clone(), ApiKeyUpdate::Keep))
            .unwrap();
    }
    let services = Arc::new(AppServices::open_with_credentials(temp.path(), credentials).unwrap());
    assert_eq!(services.provider_settings().unwrap(), settings);
    let conversation =
        AgentConversation::new(Arc::clone(&services), ChatWindowKind::Library, None).unwrap();
    let unset = ChatGenerationSettings {
        temperature: None,
        top_p: None,
        max_output_tokens: 65_536,
        presence_penalty: None,
        frequency_penalty: None,
    };
    services.runtime().block_on(async {
        let mut prepared = Some(conversation.prepare_request(1).unwrap());
        let mut next = settings.clone();
        next.chat_generation = unset.clone();
        services
            .configure_provider(next, ApiKeyUpdate::Keep)
            .await
            .unwrap();
        for (request_id, expected_answer) in [
            (1, "使用已保存的对话参数。"),
            (2, "使用下一次保存的对话参数。"),
        ] {
            let question = ConversationQuestion {
                request_id,
                question: "说明当前对话参数。".to_string(),
                allowed_book_ids: Vec::new(),
                book_titles: Vec::new(),
                snapshots: Vec::new(),
                truncate_from: None,
            };
            let result = match prepared.take() {
                Some(prepared) => conversation.ask_prepared(question, None, prepared).await,
                None => conversation.ask(question, None).await,
            }
            .unwrap();
            assert_eq!(result.answer.markdown, expected_answer);
            assert_eq!(
                result.answer.source_status,
                AgentAnswerSourceStatus::NoVerifiedSources
            );
        }
    });
    server.join().unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    let first = chat_request_body(&requests[0]);
    assert_eq!(first["model"], "configured-chat-model");
    assert_chat_sampling_parameters(&first, &settings.chat_generation);
    assert_eq!(
        first["max_tokens"],
        settings.chat_generation.max_output_tokens
    );
    assert_eq!(first["max_tokens"], 131_072);
    let second = chat_request_body(&requests[1]);
    assert_chat_sampling_parameters(&second, &unset);
    assert_eq!(second["max_tokens"], 65_536);
}
