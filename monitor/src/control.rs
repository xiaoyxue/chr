//! The monitor's own JSON-RPC endpoint: WebRTC signaling and monitor status.
//! It has no access to program control; that stays on `chr-server`.

use std::sync::Arc;

use chr_common::{
    jsonrpc::{ErrorObject, INVALID_PARAMS, SERVER_BUSY},
    methods::{
        monitor,
        webrtc::{self, SdpType, SessionDescription},
    },
    server::{ConnectionInfo, Handler},
};
use serde_json::Value;

use crate::{
    Counters,
    webrtc::{WebRtcError, WebRtcHub},
};

pub struct MonitorHandler {
    webrtc: Arc<WebRtcHub>,
    counters: Arc<Counters>,
}

impl MonitorHandler {
    pub fn new(webrtc: Arc<WebRtcHub>, counters: Arc<Counters>) -> Self {
        Self { webrtc, counters }
    }

    async fn offer(
        &self,
        connection: &ConnectionInfo,
        params: Option<Value>,
    ) -> Result<SessionDescription, ErrorObject> {
        let params = params.ok_or_else(|| ErrorObject::invalid_params("params are required"))?;
        let offer: SessionDescription = serde_json::from_value(params)
            .map_err(|error| ErrorObject::invalid_params(error.to_string()))?;
        if offer.sdp_type != SdpType::Offer {
            return Err(ErrorObject::invalid_params("type must be \"offer\""));
        }
        let sdp =
            self.webrtc
                .answer(connection.id, offer.sdp)
                .await
                .map_err(|error| match error {
                    WebRtcError::TooManyPeers => ErrorObject::new(SERVER_BUSY, error.to_string()),
                    WebRtcError::InvalidOffer(_) => {
                        ErrorObject::new(INVALID_PARAMS, error.to_string())
                    }
                    WebRtcError::GatheringTimeout | WebRtcError::Stack(_) => {
                        ErrorObject::internal(error.to_string())
                    }
                })?;
        Ok(SessionDescription {
            sdp_type: SdpType::Answer,
            sdp,
        })
    }
}

impl Handler for MonitorHandler {
    async fn handle(
        &self,
        connection: &ConnectionInfo,
        method: &str,
        params: Option<Value>,
    ) -> Result<Value, ErrorObject> {
        let result = match method {
            webrtc::OFFER => serde_json::to_value(self.offer(connection, params).await?),
            monitor::GET_STATUS => {
                if params
                    .as_ref()
                    .is_some_and(|params| params != &Value::Object(Default::default()))
                {
                    return Err(ErrorObject::invalid_params(
                        "monitor.get_status takes no params",
                    ));
                }
                serde_json::to_value(self.counters.status(self.webrtc.viewers()))
            }
            _ => return Err(ErrorObject::method_not_found(method)),
        };
        result.map_err(|error| ErrorObject::internal(error.to_string()))
    }

    fn disconnected(&self, connection: &ConnectionInfo) {
        self.webrtc.close_connection(connection.id);
    }
}

#[cfg(test)]
mod tests {
    use chr_common::{
        jsonrpc::METHOD_NOT_FOUND,
        methods::{sdi, triangle},
        server::{ConnectionId, Transport},
    };
    use serde_json::json;

    use super::*;
    use crate::{encoder::VideoFeed, webrtc::IceConfig};

    const CONNECTION: ConnectionInfo = ConnectionInfo {
        id: ConnectionId(1),
        transport: Transport::WebSocket,
    };

    fn handler() -> MonitorHandler {
        let hub = Arc::new(WebRtcHub::new(
            Arc::new(VideoFeed::new()),
            30,
            IceConfig::default(),
        ));
        MonitorHandler::new(hub, Arc::default())
    }

    #[tokio::test]
    async fn reports_status_and_rejects_bad_offers_and_control_methods() {
        let handler = handler();
        let status = handler
            .handle(&CONNECTION, monitor::GET_STATUS, None)
            .await
            .unwrap();
        assert_eq!(
            status,
            json!({"frames_encoded": 0, "frames_skipped": 0, "viewers": 0, "srt_callers": 0,
                "encoder": "", "preview_width": 0, "preview_height": 0})
        );
        for (method, params) in [
            (monitor::GET_STATUS, Some(json!({"x": 1}))),
            (webrtc::OFFER, None),
            (webrtc::OFFER, Some(json!({"type": "answer", "sdp": "v=0"}))),
            (
                webrtc::OFFER,
                Some(json!({"type": "offer", "sdp": "not sdp"})),
            ),
        ] {
            let error = handler
                .handle(&CONNECTION, method, params.clone())
                .await
                .unwrap_err();
            assert_eq!(error.code, INVALID_PARAMS, "{method} {params:?}");
        }
        // The monitor never controls the program.
        for method in [sdi::GET_STATUS, triangle::START_ROTATION] {
            let error = handler.handle(&CONNECTION, method, None).await.unwrap_err();
            assert_eq!(error.code, METHOD_NOT_FOUND, "{method}");
        }
    }
}
