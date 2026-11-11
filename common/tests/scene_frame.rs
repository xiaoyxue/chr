use chr_common::{
    jsonrpc::{ErrorObject, INVALID_PARAMS},
    scene_frame::*,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

fn fixtures() -> Value {
    let mut fixtures: Value =
        serde_json::from_str(include_str!("fixtures/scene_frame.json")).unwrap();
    for positive in fixtures["valid_changes"].as_array().unwrap().clone() {
        let mut call = case(&fixtures, positive["base"].as_str().unwrap()).clone();
        let side = positive["side"].as_str().unwrap();
        call[side] = changed(&fixtures, &positive, side);
        call["name"] = positive["name"].clone();
        fixtures["calls"].as_array_mut().unwrap().push(call);
    }
    fixtures
}

fn case<'a>(fixtures: &'a Value, name: &str) -> &'a Value {
    fixtures["calls"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()
}

fn changed(fixtures: &Value, negative: &Value, side: &str) -> Value {
    if negative["path"] == "" {
        return negative["value"].clone();
    }
    let mut value = case(fixtures, negative["base"].as_str().unwrap())[side].clone();
    let (parent, field) = negative["path"].as_str().unwrap().rsplit_once('/').unwrap();
    let object = value.pointer_mut(parent).unwrap().as_object_mut().unwrap();
    if negative["duplicate"] == true {
        let list = object[field].as_array_mut().unwrap();
        list.push(list[0].clone());
    } else if negative["remove"] == true {
        assert!(object.remove(field).is_some());
    } else {
        object.insert(field.into(), negative["value"].clone());
    }
    value
}

fn round_trip<T: DeserializeOwned + Serialize>(value: &Value) {
    let parsed: T = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        numeric_normalization(serde_json::to_value(parsed).unwrap()),
        numeric_normalization(value.clone())
    );
}

// JSON has one number domain; serde_json distinguishes 1 from 1.0 internally.
// Identity, ticks and revisions remain strings and are never normalized.
fn numeric_normalization(value: Value) -> Value {
    match value {
        Value::Number(number) => json!(number.as_f64().unwrap()),
        Value::Array(values) => {
            Value::Array(values.into_iter().map(numeric_normalization).collect())
        }
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, numeric_normalization(value)))
                .collect(),
        ),
        other => other,
    }
}

#[test]
fn shared_payloads_round_trip_with_exact_results() {
    let fixtures = fixtures();
    for call in fixtures["calls"].as_array().unwrap() {
        let method = call["method"].as_str().unwrap();
        let parsed = parse_params(method, Some(call["params"].clone())).unwrap();
        assert_eq!(
            numeric_normalization(serde_json::to_value(parsed).unwrap()),
            numeric_normalization(call["params"].clone()),
            "{method}"
        );
        validate_result(method, call["result"].clone()).unwrap();
        let result = &call["result"];
        match method {
            GET_CONFIGURATION => round_trip::<Configuration>(result),
            SCENE_CREATE | SCENE_MUTATE => round_trip::<SceneApplied>(result),
            SCENE_GET => round_trip::<SceneSnapshot>(result),
            FRAME_PUBLISH => round_trip::<PublishedFrame>(result),
            RENDER_SUBMIT => round_trip::<SubmissionAccepted>(result),
            RENDER_GET => round_trip::<SubmissionSnapshot>(result),
            RENDER_CANCEL => round_trip::<CancelResult>(result),
            SCENE_RELEASE | FRAME_RELEASE | RENDER_RELEASE => round_trip::<ReleaseResult>(result),
            _ => panic!("unknown fixture method"),
        }
    }
    for method in METHODS {
        for params in [None, Some(Value::Null), Some(json!([]))] {
            assert_eq!(
                parse_params(method, params).unwrap_err().code,
                INVALID_PARAMS
            );
        }
    }
}

#[test]
fn shared_negative_payloads_reject_missing_nullable_and_excluded_fields() {
    let fixtures = fixtures();
    for negative in fixtures["invalid_params"].as_array().unwrap() {
        let method = case(&fixtures, negative["base"].as_str().unwrap())["method"]
            .as_str()
            .unwrap();
        let result = parse_params(method, Some(changed(&fixtures, negative, "params")));
        assert_eq!(
            result.unwrap_err().code,
            INVALID_PARAMS,
            "{}",
            negative["name"]
        );
    }
    for negative in fixtures["invalid_results"].as_array().unwrap() {
        let method = case(&fixtures, negative["base"].as_str().unwrap())["method"]
            .as_str()
            .unwrap();
        assert!(
            validate_result(method, changed(&fixtures, negative, "result")).is_err(),
            "{}",
            negative["name"]
        );
    }
}

#[test]
fn shared_scalar_boundaries_and_native_nonfinite_values() {
    let fixtures = fixtures();
    let scalars = &fixtures["scalars"];
    for value in scalars["u64_valid"].as_array().unwrap() {
        round_trip::<U64>(value);
    }
    for value in scalars["time_valid"].as_array().unwrap() {
        round_trip::<TimeBase>(value);
    }
    for value in scalars["u64_invalid"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<U64>(value.clone()).is_err(),
            "{value}"
        );
    }
    for value in scalars["time_invalid"].as_array().unwrap() {
        assert!(
            serde_json::from_value::<TimeBase>(value.clone()).is_err(),
            "{value}"
        );
    }
    assert_eq!(U64::from(u64::MAX).checked_add(1), None);
    assert_eq!(U64::from(0).checked_add(1), Some(U64::from(1)));
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(Finite::try_from(value).is_err());
    }
    assert!(serde_json::from_str::<Finite>("1e400").is_err());
}

#[test]
fn duplicate_lists_and_nullable_values_are_validated_without_reordering_operations() {
    let fixtures = fixtures();
    let mut create = case(&fixtures, "create")["params"].clone();
    let duplicate = create["state"]["instances"][0].clone();
    create["state"]["instances"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(parse_params(SCENE_CREATE, Some(create)).is_err());
    let mut create = case(&fixtures, "create")["params"].clone();
    let duplicate = create["state"]["lighting"]["lights"][0].clone();
    create["state"]["lighting"]["lights"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(parse_params(SCENE_CREATE, Some(create)).is_err());
    let mut create = case(&fixtures, "create")["params"].clone();
    let duplicate = create["state"]["instances"][0]["material_overrides"][0].clone();
    create["state"]["instances"][0]["material_overrides"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(parse_params(SCENE_CREATE, Some(create)).is_err());
    let mut submit = case(&fixtures, "submit")["params"].clone();
    let duplicate = submit["frames"][0].clone();
    submit["frames"].as_array_mut().unwrap().push(duplicate);
    assert!(parse_params(RENDER_SUBMIT, Some(submit)).is_err());
    let mut mutate = case(&fixtures, "mutate")["params"].clone();
    let duplicate = mutate["operations"][0].clone();
    mutate["operations"].as_array_mut().unwrap().push(duplicate);
    assert!(parse_params(SCENE_MUTATE, Some(mutate)).is_ok());
}

const KINDS: [RejectionKind; 7] = [
    RejectionKind::RuntimeChanged,
    RejectionKind::NotFound,
    RejectionKind::Conflict,
    RejectionKind::IncompatibleInput,
    RejectionKind::Capacity,
    RejectionKind::InvalidState,
    RejectionKind::UnsupportedProfile,
];

#[test]
fn rejection_codes_have_exact_kind_and_required_nullable_frame() {
    for kind in KINDS {
        for frame in [
            None,
            Some(FrameKey {
                stream_id: Id::try_from("stream".to_owned()).unwrap(),
                sequence: U64::from(u64::MAX),
            }),
        ] {
            let rejection = Rejection {
                kind,
                message: "Synthetic rejection".into(),
                frame,
            };
            let value = serde_json::to_value(&rejection).unwrap();
            assert_eq!(value["code"], kind.code());
            assert_eq!(
                serde_json::from_value::<Rejection>(value.clone()).unwrap(),
                rejection
            );
            let error: ErrorObject = rejection.into();
            assert_eq!(serde_json::to_value(error).unwrap(), value);
            let mut bad = value.clone();
            bad["code"] = json!(2000);
            assert!(serde_json::from_value::<Rejection>(bad).is_err());
            let mut bad = value.clone();
            bad["data"].as_object_mut().unwrap().remove("frame");
            assert!(serde_json::from_value::<Rejection>(bad).is_err());
            let mut bad = value;
            bad["data"]["unexpected"] = json!(true);
            assert!(serde_json::from_value::<Rejection>(bad).is_err());
        }
    }
}

#[cfg(unix)]
mod ipc {
    use std::sync::{Arc, Mutex};

    use chr_common::{
        client::NativeSocketClient,
        server::{
            ConnectionInfo, Handler, IpcServer, NativeSocketListener, ServerConfig, Transport,
            WebSocketListener,
        },
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    struct MockHandler {
        fixtures: Value,
        transports: Mutex<Vec<Transport>>,
    }

    impl Handler for MockHandler {
        async fn handle(
            &self,
            connection: &ConnectionInfo,
            method: &str,
            params: Option<Value>,
        ) -> Result<Value, ErrorObject> {
            self.transports.lock().unwrap().push(connection.transport);
            let parsed = parse_params(method, params.clone())?;
            if let Params::SceneGet(target) = parsed
                && let Some(index) = target.scene_id.as_str().strip_prefix("reject-")
            {
                let kind = KINDS[index.parse::<usize>().unwrap()];
                return Err(Rejection {
                    kind,
                    message: "Synthetic rejection".into(),
                    frame: None,
                }
                .into());
            }
            let call = self.fixtures["calls"]
                .as_array()
                .unwrap()
                .iter()
                .find(|call| call["method"] == method && Some(&call["params"]) == params.as_ref())
                .expect("mock only handles declared synthetic fixtures");
            Ok(call["result"].clone())
        }
    }

    #[tokio::test]
    async fn identical_mock_conformance_on_native_socket_and_websocket() {
        tokio::time::timeout(std::time::Duration::from_secs(30), conformance())
            .await
            .unwrap();
    }

    async fn conformance() {
        let fixtures = fixtures();
        let handler = Arc::new(MockHandler {
            fixtures: fixtures.clone(),
            transports: Mutex::new(Vec::new()),
        });
        let server = IpcServer::new(Arc::clone(&handler), ServerConfig::default());
        let directory = std::env::current_dir().unwrap().join("output");
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("dto-{}.sock", std::process::id()));
        let native_listener = NativeSocketListener::bind(&path).await.unwrap();
        let ws_listener = WebSocketListener::bind("127.0.0.1:0".parse().unwrap())
            .await
            .unwrap();
        let ws_url = format!("ws://{}", ws_listener.local_addr().unwrap());
        let native_server = server.clone();
        let native_task =
            tokio::spawn(async move { native_server.serve_native_socket(native_listener).await });
        let ws_task = tokio::spawn(async move { server.serve_websocket(ws_listener).await });
        let mut native = NativeSocketClient::connect(&path).await.unwrap();
        let (mut ws, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();

        let mut requests = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for call in fixtures["calls"].as_array().unwrap() {
            if !seen.insert((call["method"].to_string(), call["params"].to_string())) {
                continue;
            }
            requests.push((
                json!({"jsonrpc":"2.0","id":call["name"],"method":call["method"],"params":call["params"]}),
                json!({"jsonrpc":"2.0","id":call["name"],"result":call["result"]}),
            ));
        }
        for negative in fixtures["invalid_params"].as_array().unwrap() {
            let method = &case(&fixtures, negative["base"].as_str().unwrap())["method"];
            requests.push((
                json!({"jsonrpc":"2.0","id":negative["name"],"method":method,"params":changed(&fixtures, negative, "params")}),
                Value::Null,
            ));
        }
        for (index, kind) in KINDS.iter().enumerate() {
            let error: ErrorObject = Rejection {
                kind: *kind,
                message: "Synthetic rejection".into(),
                frame: None,
            }
            .into();
            requests.push((
                json!({"jsonrpc":"2.0","id":index,"method":SCENE_GET,"params":{"runtime_id":"runtime-01","scene_id":format!("reject-{index}")}}),
                json!({"jsonrpc":"2.0","id":index,"error":error}),
            ));
        }
        // JSON-RPC batch retains independent results; it is not scene.mutate.
        requests.push((
            json!([requests[0].0, {"jsonrpc":"2.0","id":"bad","method":RENDER_SUBMIT,"params":{}}]),
            Value::Null,
        ));
        for (request, expected) in requests {
            let text = request.to_string();
            let native_reply: Value =
                serde_json::from_slice(&native.send_raw(text.as_bytes()).await.unwrap().unwrap())
                    .unwrap();
            ws.send(Message::text(text)).await.unwrap();
            let Message::Text(text) = ws.next().await.unwrap().unwrap() else {
                panic!("expected text reply")
            };
            let ws_reply: Value = serde_json::from_str(text.as_str()).unwrap();
            assert_eq!(native_reply, ws_reply, "{request}");
            if !expected.is_null() {
                assert_eq!(native_reply, expected);
            } else if request.is_array() {
                assert!(native_reply[0]["result"].is_object());
                assert_eq!(native_reply[1]["error"]["code"], INVALID_PARAMS);
            } else {
                assert_eq!(native_reply["error"]["code"], INVALID_PARAMS, "{request}");
            }
        }
        ws.close(None).await.unwrap();
        drop(native);
        {
            let transports = handler.transports.lock().unwrap();
            assert!(transports.contains(&Transport::NativeSocket));
            assert!(transports.contains(&Transport::WebSocket));
        }
        native_task.abort();
        ws_task.abort();
        let _ = native_task.await;
        let _ = ws_task.await;
        assert!(!path.exists());
    }
}
