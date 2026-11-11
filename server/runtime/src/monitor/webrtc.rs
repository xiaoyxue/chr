//! WebRTC preview: answers browser offers and streams encoded frames to each peer.
//!
//! Signaling is non-trickle. The answer is returned after ICE gathering completes, so it
//! already lists every server candidate. No STUN/TURN servers are configured: clients reach
//! the host candidates directly. ICE-TCP on a fixed loopback port range lets a browser on
//! another machine connect through an SSH port forward, which carries TCP only.

use std::{
    collections::HashMap,
    hash::{BuildHasher, Hasher},
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use chr_common::server::ConnectionId;
use rtc::{
    media::Sample,
    media_stream::MediaStreamTrack,
    peer_connection::configuration::media_engine::MIME_TYPE_H264,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodecParameters, RTCRtpCodingParameters, RTCRtpEncodingParameters,
        RtpCodecKind,
    },
};
use tokio::sync::{Notify, broadcast, watch};
use webrtc::{
    media_stream::track_local::{TrackLocal, static_sample::TrackLocalStaticSample},
    peer_connection::{
        MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
        RTCConfigurationBuilder, RTCIceGatheringState, RTCPeerConnectionState,
        RTCSessionDescription, Registry, SettingEngineBuilder, register_default_interceptors,
    },
    rtp_transceiver::RtpSender,
};

use super::encoder::VideoFeed;

/// Upper bound for simultaneous previews; each costs a peer connection and a stream task.
pub const MAX_PEERS: usize = 4;
const GATHERING_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Constrained Baseline, the profile every WebRTC browser decodes in hardware or software.
const H264_FMTP: &str = "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f";
const H264_PAYLOAD_TYPE: u8 = 102;

#[derive(Debug, thiserror::Error)]
pub enum WebRtcError {
    #[error("too many active previews (limit {MAX_PEERS})")]
    TooManyPeers,
    #[error("invalid offer: {0}")]
    InvalidOffer(String),
    #[error("ICE gathering did not complete within {GATHERING_TIMEOUT:?}")]
    GatheringTimeout,
    #[error("webrtc: {0}")]
    Stack(#[from] webrtc::error::Error),
}

type Peer = Arc<dyn PeerConnection>;

struct Session {
    id: u64,
    peer: Peer,
}

/// Where peers listen for ICE.
#[derive(Debug, Clone, Default)]
pub struct IceConfig {
    /// UDP bind address; a wildcard binds every non-loopback interface. `None` disables UDP.
    pub udp: Option<String>,
    /// First ICE-TCP port; peer slot `n` listens on `port + n`, so a forward of
    /// [`MAX_PEERS`] consecutive ports covers every preview. `None` disables TCP.
    pub tcp_base: Option<SocketAddr>,
}

impl IceConfig {
    fn tcp_addr(&self, slot: usize) -> Option<String> {
        self.tcp_base
            .map(|base| SocketAddr::new(base.ip(), base.port() + slot as u16).to_string())
    }
}

pub struct WebRtcHub {
    feed: Arc<VideoFeed>,
    fps: u32,
    ice: IceConfig,
    state: Mutex<HubState>,
}

#[derive(Default)]
struct HubState {
    next_session: u64,
    /// A slot is held from the offer until its stream task ends, bounding active peers
    /// and naming each peer's ICE-TCP port.
    slots: [bool; MAX_PEERS],
    sessions: HashMap<ConnectionId, Vec<Session>>,
}

/// Forwards the events this module waits on.
struct PeerEvents {
    gathered: Arc<Notify>,
    state: watch::Sender<RTCPeerConnectionState>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for PeerEvents {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        log::debug!("peer connection state: {state}");
        let _ = self.state.send(state);
    }
}

impl WebRtcHub {
    /// Every peer streams `feed`, the encoded program output.
    pub fn new(feed: Arc<VideoFeed>, fps: u32, ice: IceConfig) -> Self {
        Self {
            feed,
            fps,
            ice,
            state: Mutex::new(HubState::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState> {
        // A panic while holding this lock cannot leave the map half-updated.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Creates a peer for `offer_sdp` streaming the program output, and returns the answer SDP. The
    /// peer is owned by `connection` and closed when that control connection goes away.
    pub async fn answer(
        self: &Arc<Self>,
        connection: ConnectionId,
        offer_sdp: String,
    ) -> Result<String, WebRtcError> {
        let (session_id, slot) = {
            let mut state = self.lock();
            let slot = state
                .slots
                .iter()
                .position(|used| !used)
                .ok_or(WebRtcError::TooManyPeers)?;
            state.slots[slot] = true;
            state.next_session += 1;
            (state.next_session, slot)
        };
        match self.start(connection, offer_sdp, session_id, slot).await {
            Ok(answer) => Ok(answer),
            Err(error) => {
                self.release(slot);
                Err(error)
            }
        }
    }

    async fn start(
        self: &Arc<Self>,
        connection: ConnectionId,
        offer_sdp: String,
        session_id: u64,
        slot: usize,
    ) -> Result<String, WebRtcError> {
        let offer = RTCSessionDescription::offer(offer_sdp)
            .map_err(|error| WebRtcError::InvalidOffer(error.to_string()))?;

        let codec = RTCRtpCodec {
            mime_type: MIME_TYPE_H264.to_owned(),
            clock_rate: 90_000,
            channels: 0,
            sdp_fmtp_line: H264_FMTP.to_owned(),
            rtcp_feedback: vec![],
        };
        let mut media_engine = MediaEngine::default();
        media_engine.register_codec(
            RTCRtpCodecParameters {
                rtp_codec: codec.clone(),
                payload_type: H264_PAYLOAD_TYPE,
            },
            RtpCodecKind::Video,
        )?;
        let registry = register_default_interceptors(Registry::new(), &mut media_engine)?;

        let gathered = Arc::new(Notify::new());
        let (state_tx, state_rx) = watch::channel(RTCPeerConnectionState::New);
        let peer: Peer = Arc::new(
            PeerConnectionBuilder::new()
                .with_configuration(RTCConfigurationBuilder::new().build())
                .with_media_engine(media_engine)
                .with_interceptor_registry(registry)
                .with_handler(Arc::new(PeerEvents {
                    gathered: Arc::clone(&gathered),
                    state: state_tx,
                }))
                // Loopback candidates are what an SSH-forwarded client can reach.
                .with_setting_engine(
                    SettingEngineBuilder::new()
                        .with_include_loopback_candidate(true)
                        .build(),
                )
                .with_udp_addrs(self.ice.udp.iter().cloned().collect())
                .with_tcp_addrs(self.ice.tcp_addr(slot).into_iter().collect())
                .build()
                .await?,
        );

        let result = self.negotiate(&peer, &codec, offer, &gathered).await;
        let (track, sender, ssrc, answer) = match result {
            Ok(parts) => parts,
            Err(error) => {
                let _ = peer.close().await;
                return Err(error);
            }
        };

        self.lock()
            .sessions
            .entry(connection)
            .or_default()
            .push(Session {
                id: session_id,
                peer: Arc::clone(&peer),
            });
        log::info!("{connection} started preview session {session_id}");

        let hub = Arc::clone(self);
        let feed = Arc::clone(&self.feed);
        tokio::spawn(async move {
            if let Err(error) = hub.stream(&feed, &track, &sender, ssrc, state_rx).await {
                log::warn!("preview session {session_id} stopped: {error}");
            }
            let _ = peer.close().await;
            hub.remove(connection, session_id);
            hub.release(slot);
            log::info!("{connection} ended preview session {session_id}");
        });
        Ok(answer)
    }

    async fn negotiate(
        &self,
        peer: &Peer,
        codec: &RTCRtpCodec,
        offer: RTCSessionDescription,
        gathered: &Notify,
    ) -> Result<(Arc<TrackLocalStaticSample>, Arc<dyn RtpSender>, u32, String), WebRtcError> {
        let ssrc = random_ssrc();
        let track = Arc::new(TrackLocalStaticSample::new(
            Instant::now(),
            MediaStreamTrack::new(
                "chr-preview".to_owned(),
                "chr-triangle".to_owned(),
                "chr rotating triangle".to_owned(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(ssrc),
                        ..Default::default()
                    },
                    codec: codec.clone(),
                    ..Default::default()
                }],
            ),
        )?);
        let sender = peer
            .add_track(Arc::clone(&track) as Arc<dyn TrackLocal>)
            .await?;
        peer.set_remote_description(offer)
            .await
            .map_err(|error| WebRtcError::InvalidOffer(error.to_string()))?;
        let answer = peer.create_answer(None).await?;
        peer.set_local_description(answer).await?;
        tokio::time::timeout(GATHERING_TIMEOUT, gathered.notified())
            .await
            .map_err(|_| WebRtcError::GatheringTimeout)?;
        let answer = peer
            .local_description()
            .await
            .ok_or_else(|| WebRtcError::InvalidOffer("no local description".into()))?;
        Ok((track, sender, ssrc, answer.sdp))
    }

    /// Writes frames to one peer until it closes, fails, or never connects.
    async fn stream(
        &self,
        feed: &VideoFeed,
        track: &TrackLocalStaticSample,
        sender: &Arc<dyn RtpSender>,
        ssrc: u32,
        mut state: watch::Receiver<RTCPeerConnectionState>,
    ) -> Result<(), String> {
        let connected = *tokio::time::timeout(
            CONNECT_TIMEOUT,
            state.wait_for(|state| {
                matches!(
                    state,
                    RTCPeerConnectionState::Connected
                        | RTCPeerConnectionState::Failed
                        | RTCPeerConnectionState::Closed
                )
            }),
        )
        .await
        .map_err(|_| "peer did not connect in time".to_owned())?
        .map_err(|_| "peer events ended".to_owned())?;
        if connected != RTCPeerConnectionState::Connected {
            return Err(format!("peer {connected} before connecting"));
        }
        let payload_type = sender
            .get_parameters()
            .await
            .map_err(|error| error.to_string())?
            .rtp_parameters
            .codecs
            .first()
            .map(|codec| codec.payload_type)
            .ok_or("no negotiated video codec")?;

        let frame_duration = Duration::from_secs_f64(1.0 / f64::from(self.fps));
        let mut frames = feed.subscribe();
        feed.request_keyframe();
        let mut need_keyframe = true;
        let mut last_frame: Option<u64> = None;
        loop {
            tokio::select! {
                changed = state.changed() => {
                    if changed.is_err() {
                        return Ok(());
                    }
                    let current = *state.borrow();
                    if matches!(current, RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed) {
                        return Ok(());
                    }
                }
                frame = frames.recv() => match frame {
                    Ok(frame) => {
                        if need_keyframe && !frame.keyframe {
                            continue;
                        }
                        need_keyframe = false;
                        // RTP time follows logical frames, so dropped frames keep their slot.
                        let elapsed_frames = last_frame
                            .map_or(1, |last| frame.frame.saturating_sub(last).max(1));
                        last_frame = Some(frame.frame);
                        let sample = Sample {
                            data: frame.data.clone(),
                            duration: frame_duration * elapsed_frames as u32,
                            ..Sample::new(Instant::now())
                        };
                        track
                            .sample_writer(ssrc, payload_type)
                            .write_sample(&sample)
                            .await
                            .map_err(|error| error.to_string())?;
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // Predicted frames are useless after a gap; resume at the next IDR.
                        log::debug!("preview viewer lagged by {skipped} frames");
                        need_keyframe = true;
                        feed.request_keyframe();
                    }
                    Err(broadcast::error::RecvError::Closed) => return Ok(()),
                }
            }
        }
    }

    fn release(&self, slot: usize) {
        self.lock().slots[slot] = false;
    }

    fn remove(&self, connection: ConnectionId, session_id: u64) {
        let mut state = self.lock();
        if let Some(sessions) = state.sessions.get_mut(&connection) {
            sessions.retain(|session| session.id != session_id);
            if sessions.is_empty() {
                state.sessions.remove(&connection);
            }
        }
    }

    /// Closes every preview created through `connection`.
    pub fn close_connection(&self, connection: ConnectionId) {
        let sessions = self.lock().sessions.remove(&connection).unwrap_or_default();
        for session in sessions {
            tokio::spawn(async move {
                let _ = session.peer.close().await;
            });
        }
    }
}

/// SSRCs only need to be unlikely to collide; the std hasher is randomly seeded.
fn random_ssrc() -> u32 {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos()),
    );
    hasher.finish() as u32
}
